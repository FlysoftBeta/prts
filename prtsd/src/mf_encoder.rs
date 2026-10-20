//! Hardware H.264 MFT, with a dedicated COM MTA and bounded synchronous requests.
//! CPU NV12 samples and GPU NV12 DXGI surface samples share the same async
//! transform, rate control, bounded requests and output publication.
use crate::encoder::VideoEncoder;
use anyhow::{Context, Result, bail, ensure};
use std::{
    mem::ManuallyDrop,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Media::MediaFoundation::*,
        System::{Com::*, Variant::VARIANT},
    },
    core::{GUID, Interface},
};

enum Request {
    Encode(Vec<u8>, bool, mpsc::SyncSender<Result<Vec<u8>>>),
    Texture(crate::d3d::Frame, bool, mpsc::SyncSender<Result<Vec<u8>>>),
    Bitrate(u32, mpsc::SyncSender<Result<()>>),
}
pub struct MfEncoder {
    tx: Option<mpsc::SyncSender<Request>>,
    worker: Option<thread::JoinHandle<()>>,
    name: String,
    width: usize,
    height: usize,
}
impl MfEncoder {
    pub fn new(width: u32, height: u32, bps: u32, fps: u32) -> Result<Self> {
        ensure!(
            width > 0
                && height > 0
                && width <= 4096
                && height <= 4096
                && width.is_multiple_of(2)
                && height.is_multiple_of(2)
                && fps > 0
                && fps <= 60,
            "invalid MF dimensions/fps"
        );
        let (tx, rx) = mpsc::sync_channel(1);
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let worker = thread::Builder::new()
            .name("prts-mf-encoder".into())
            .spawn(move || unsafe {
                let init = CoInitializeEx(None, COINIT_MULTITHREADED).ok();
                if let Err(e) = init {
                    let _ = ready_tx.send(Err(anyhow::Error::from(e)));
                    return;
                }
                if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                    let _ = ready_tx.send(Err(e.into()));
                    CoUninitialize();
                    return;
                }
                match Hardware::new(width, height, bps, fps) {
                    Ok(mut hw) => {
                        let _ = ready_tx.send(Ok(hw.name.clone()));
                        while let Ok(request) = rx.recv() {
                            match request {
                                Request::Encode(data, key, reply) => {
                                    let _ = reply.send(hw.encode(&data, key));
                                }
                                Request::Texture(frame, key, reply) => {
                                    let _ = reply.send(hw.encode_input(&[], Some(&frame), key));
                                }
                                Request::Bitrate(bps, reply) => {
                                    let _ = reply.send(hw.reconfigure_bitrate(bps));
                                }
                            }
                        }
                        hw.shutdown();
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
                let _ = MFShutdown();
                CoUninitialize();
            })?;
        match ready_rx.recv()? {
            Ok(name) => Ok(Self {
                tx: Some(tx),
                worker: Some(worker),
                name,
                width: width as usize,
                height: height as usize,
            }),
            Err(e) => {
                let _ = worker.join();
                Err(e)
            }
        }
    }
    pub fn encode_texture(&mut self, frame: &crate::d3d::Frame, key: bool) -> Result<Vec<u8>> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx
            .as_ref()
            .unwrap()
            .send(Request::Texture(frame.clone(), key, tx))?;
        rx.recv()?
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn set_bitrate(&mut self, bps: u32) -> Result<()> {
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx.as_ref().unwrap().send(Request::Bitrate(bps, tx))?;
        rx.recv()?
    }
}
impl VideoEncoder for MfEncoder {
    fn encode(
        &mut self,
        bgra: &[u8],
        width: usize,
        height: usize,
        keyframe: bool,
    ) -> Result<Vec<u8>> {
        ensure!(
            width == self.width && height == self.height && bgra.len() == width * height * 4,
            "MF input dimensions changed"
        );
        let (tx, rx) = mpsc::sync_channel(1);
        self.tx
            .as_ref()
            .unwrap()
            .send(Request::Encode(bgra.to_vec(), keyframe, tx))?;
        rx.recv()?
    }
}
impl Drop for MfEncoder {
    fn drop(&mut self) {
        self.tx.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

struct Hardware {
    activation: IMFActivate,
    manager: Option<IMFDXGIDeviceManager>,
    mft: IMFTransform,
    events: IMFMediaEventGenerator,
    codec: ICodecAPI,
    name: String,
    width: u32,
    height: u32,
    fps: u32,
    frame: i64,
    input_credit: usize,
    output_credit: usize,
    input_id: u32,
    output_id: u32,
    last_output: Instant,
    bitrate: u32,
    rate_bytes: u64,
    rate_frames: u32,
}
impl Hardware {
    fn shutdown(&self) {
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            if let Ok(shutdown) = self.mft.cast::<IMFShutdown>() {
                let _ = shutdown.Shutdown();
            }
            // Match ActivateObject with ShutdownObject as well as releasing COM
            // interfaces. The Intel registry watcher leak is tracked by M4's
            // lifetime acceptance; these successful calls do not resolve it.
            let _ = self.activation.ShutdownObject();
        }
    }
    fn reconfigure_bitrate(&mut self, bps: u32) -> Result<()> {
        self.set(&CODECAPI_AVEncCommonMeanBitRate, VARIANT::from(bps))?;
        // UHD 630's CPU-input Intel MFT accepts the live CODECAPI change but
        // keeps emitting at the old MF_MT_AVG_BITRATE (including CBR filler).
        // Recreate the stream with matching CODECAPI and media-type values.
        // Preserve the public encoder instance and emit a new SPS/PPS + IDR.
        // This conservative path applies to all drivers until live updates are
        // qualified per driver; successful SetValue alone is not evidence.
        let replacement = unsafe { Self::new(self.width, self.height, bps, self.fps)? };
        self.shutdown();
        *self = replacement;
        Ok(())
    }
    unsafe fn new(width: u32, height: u32, bps: u32, fps: u32) -> Result<Self> {
        unsafe {
            let input = MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Video,
                guidSubtype: MFVideoFormat_NV12,
            };
            let output = MFT_REGISTER_TYPE_INFO {
                guidMajorType: MFMediaType_Video,
                guidSubtype: MFVideoFormat_H264,
            };
            let (mut ptr, mut count) = (std::ptr::null_mut(), 0);
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input),
                Some(&output),
                &mut ptr,
                &mut count,
            )?;
            let mut candidates = Vec::new();
            for i in 0..count as usize {
                if let Some(a) = (*ptr.add(i)).take() {
                    candidates.push(a);
                }
            }
            CoTaskMemFree(Some(ptr.cast()));
            let string = |a: &IMFActivate, key: &GUID| -> String {
                let len = a.GetStringLength(key).unwrap_or(0);
                let mut data = vec![0; len as usize + 1];
                if a.GetString(key, &mut data, None).is_ok() {
                    String::from_utf16_lossy(&data[..len as usize])
                } else {
                    String::new()
                }
            };
            candidates.sort_by_key(|a| {
                let vendor = string(a, &MFT_ENUM_HARDWARE_VENDOR_ID_Attribute).to_ascii_lowercase();
                let name = string(a, &MFT_FRIENDLY_NAME_Attribute).to_ascii_lowercase();
                !(vendor.contains("8086") || name.contains("intel") || name.contains("quick sync"))
            });
            let mut errors = Vec::new();
            for activate in candidates {
                let name = string(&activate, &MFT_FRIENDLY_NAME_Attribute);
                match Self::activate(&activate, name.clone(), width, height, bps, fps) {
                    Ok(hw) => return Ok(hw),
                    Err(e) => {
                        errors.push(format!("{name}: {e:#}"));
                        let _ = activate.ShutdownObject();
                    }
                }
            }
            bail!("no usable hardware H.264 MFT: {}", errors.join("; "))
        }
    }
    unsafe fn activate(
        activate: &IMFActivate,
        name: String,
        width: u32,
        height: u32,
        bps: u32,
        fps: u32,
    ) -> Result<Self> {
        unsafe {
            let mft: IMFTransform = activate.ActivateObject()?;
            let attrs = mft.GetAttributes()?;
            ensure!(
                attrs.GetUINT32(&MF_TRANSFORM_ASYNC)? != 0,
                "hardware MFT is not asynchronous"
            );
            attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            let codec = mft.cast::<ICodecAPI>()?;
            let events = mft.cast::<IMFMediaEventGenerator>()?;
            let (mut input_ids, mut output_ids) = ([0], [0]);
            if let Err(e) = mft.GetStreamIDs(&mut input_ids, &mut output_ids)
                && e.code() != windows::Win32::Foundation::E_NOTIMPL
            {
                return Err(e.into());
            }
            let manager = if attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) != 0 {
                if let Ok(gpu) = crate::d3d::shared() {
                    let mut token = 0;
                    let mut manager = None;
                    MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
                    let manager = manager.context("Missing DXGI device manager")?;
                    manager.ResetDevice(&gpu.lock().unwrap().device, token)?;
                    mft.ProcessMessage(
                        MFT_MESSAGE_SET_D3D_MANAGER,
                        Interface::as_raw(&manager) as usize,
                    )?;
                    Some(manager)
                } else {
                    None
                }
            } else {
                None
            };
            let mut hw = Self {
                activation: activate.clone(),
                manager,
                mft,
                events,
                codec,
                name,
                width,
                height,
                fps,
                frame: 0,
                input_credit: 0,
                output_credit: 0,
                input_id: input_ids[0],
                output_id: output_ids[0],
                last_output: Instant::now(),
                bitrate: bps,
                rate_bytes: 0,
                rate_frames: 0,
            };
            // These controls are mandatory for this backend: reject incompatible MFTs.
            hw.set(&CODECAPI_AVLowLatencyMode, VARIANT::from(true))
                .context("low latency")?;
            hw.set(
                &CODECAPI_AVEncCommonRateControlMode,
                VARIANT::from(eAVEncCommonRateControlMode_CBR.0 as u32),
            )
            .context("CBR")?;
            hw.set(&CODECAPI_AVEncMPVDefaultBPictureCount, VARIANT::from(0u32))
                .context("no B pictures")?;
            hw.set(&CODECAPI_AVEncMPVGOPSize, VARIANT::from(fps * 2))
                .context("GOP")?;
            hw.set(&CODECAPI_AVEncCommonMeanBitRate, VARIANT::from(bps))
                .context("bitrate")?;
            hw.set(&CODECAPI_AVEncCommonMaxBitRate, VARIANT::from(bps))
                .context("peak bitrate")?;
            let _ = hw.set(&CODECAPI_AVEncVideoMaxQP, VARIANT::from(51u32));
            let _ = hw.set(&CODECAPI_AVEncCommonBufferSize, VARIANT::from(bps / 4));
            let _ = hw.set(
                &CODECAPI_AVScenarioInfo,
                VARIANT::from(eAVScenarioInfo_DisplayRemoting.0 as u32),
            );
            let out = hw.media_type(MFVideoFormat_H264)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, bps)?;
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base.0 as u32)?;
            hw.mft
                .SetOutputType(hw.output_id, &out, 0)
                .context("H264 output type")?;
            let input = hw.media_type(MFVideoFormat_NV12)?;
            input.SetUINT32(&MF_MT_DEFAULT_STRIDE, width)?;
            input.SetUINT32(&MF_MT_SAMPLE_SIZE, width * height * 3 / 2)?;
            hw.mft
                .SetInputType(hw.input_id, &input, 0)
                .context("NV12 input type")?;
            hw.set(&CODECAPI_AVEncVideoMaxQP, VARIANT::from(51u32))?;
            hw.set(&CODECAPI_AVEncCommonMeanBitRate, VARIANT::from(bps))?;
            hw.mft
                .ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            hw.mft
                .ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(hw)
        }
    }
    fn set(&mut self, key: &GUID, value: VARIANT) -> Result<()> {
        unsafe {
            self.codec.SetValue(key, &value)?;
        }
        Ok(())
    }
    unsafe fn media_type(&self, subtype: GUID) -> Result<IMFMediaType> {
        unsafe {
            let t = MFCreateMediaType()?;
            t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            t.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
            t.SetUINT64(
                &MF_MT_FRAME_SIZE,
                ((self.width as u64) << 32) | self.height as u64,
            )?;
            t.SetUINT64(&MF_MT_FRAME_RATE, ((self.fps as u64) << 32) | 1)?;
            t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
            t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            Ok(t)
        }
    }
    unsafe fn poll_events(&mut self) -> Result<()> {
        unsafe {
            loop {
                match self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) {
                    Ok(event) => {
                        event.GetStatus()?.ok()?;
                        let kind = event.GetType()?;
                        if kind == METransformNeedInput.0 as u32 {
                            self.input_credit += 1;
                        }
                        if kind == METransformHaveOutput.0 as u32 {
                            self.output_credit += 1;
                        }
                    }
                    Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
                    Err(e) => return Err(e.into()),
                }
            }
        }
    }
    unsafe fn cpu_sample(&self, bgra: &[u8]) -> Result<IMFSample> {
        unsafe {
            use openh264::formats::YUVSource;
            let yuv = crate::encoder::bgra_to_i420(bgra, self.width as usize, self.height as usize);
            let n = (self.width * self.height) as usize;
            let buffer = MFCreateMemoryBuffer((n * 3 / 2) as u32)?;
            let mut data = std::ptr::null_mut();
            buffer.Lock(&mut data, None, None)?;
            std::ptr::copy_nonoverlapping(yuv.y().as_ptr(), data, n);
            for i in 0..n / 4 {
                *data.add(n + i * 2) = yuv.u()[i];
                *data.add(n + i * 2 + 1) = yuv.v()[i];
            }
            buffer.Unlock()?;
            buffer.SetCurrentLength((n * 3 / 2) as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(self.frame * 10_000_000 / self.fps as i64)?;
            sample.SetSampleDuration(10_000_000 / self.fps as i64)?;
            Ok(sample)
        }
    }
    fn encode(&mut self, bgra: &[u8], key: bool) -> Result<Vec<u8>> {
        self.encode_input(bgra, None, key)
    }
    fn encode_input(
        &mut self,
        bgra: &[u8],
        texture: Option<&crate::d3d::Frame>,
        key: bool,
    ) -> Result<Vec<u8>> {
        unsafe {
            let mut pending = Vec::new();
            let deadline = Instant::now() + Duration::from_secs(2);
            while self.input_credit == 0 {
                self.poll_events()?;
                while self.output_credit > 0 {
                    pending.extend(self.output()?);
                }
                ensure!(
                    Instant::now() < deadline,
                    "hardware MFT input timeout at frame {}",
                    self.frame
                );
                if self.input_credit == 0 {
                    thread::sleep(Duration::from_millis(1));
                }
            }
            if key {
                self.set(&CODECAPI_AVEncVideoForceKeyFrame, VARIANT::from(1u32))?;
            }
            let sample = if let Some(frame) = texture {
                ensure!(self.manager.is_some(), "MFT does not support D3D11 input");
                let gpu = frame.gpu.lock().unwrap();
                let nv12 = gpu.nv12(&frame.texture, self.width, self.height)?;
                let buffer = MFCreateDXGISurfaceBuffer(
                    &windows::Win32::Graphics::Direct3D11::ID3D11Texture2D::IID,
                    &nv12,
                    0,
                    false,
                )?;
                let sample = MFCreateSample()?;
                sample.AddBuffer(&buffer)?;
                sample.SetSampleTime(self.frame * 10_000_000 / self.fps as i64)?;
                sample.SetSampleDuration(10_000_000 / self.fps as i64)?;
                sample
            } else {
                self.cpu_sample(bgra)?
            };
            self.mft.ProcessInput(self.input_id, &sample, 0)?;
            self.input_credit -= 1;
            self.frame += 1;
            if !pending.is_empty() {
                return Ok(pending);
            }
            // Low-latency encoders normally return the submitted frame immediately.
            // Permit pipeline startup without ever queueing another CPU BGRA frame.
            let deadline = Instant::now() + Duration::from_millis(100);
            loop {
                self.poll_events()?;
                if self.output_credit > 0 {
                    pending.extend(self.output()?);
                    if !pending.is_empty() {
                        return Ok(pending);
                    }
                }
                if Instant::now() >= deadline {
                    ensure!(
                        self.last_output.elapsed() < Duration::from_secs(2),
                        "hardware MFT output timeout"
                    );
                    return Ok(pending);
                }
                thread::sleep(Duration::from_millis(1));
            }
        }
    }
    unsafe fn output(&mut self) -> Result<Vec<u8>> {
        unsafe {
            let info = self.mft.GetOutputStreamInfo(self.output_id)?;
            let sample = if info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 == 0 {
                let sample = MFCreateSample()?;
                sample.AddBuffer(&MFCreateMemoryBuffer(
                    info.cbSize.max(self.width * self.height),
                )?)?;
                Some(sample)
            } else {
                None
            };
            let mut out = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: self.output_id,
                pSample: ManuallyDrop::new(sample),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0;
            let result = self.mft.ProcessOutput(0, &mut out, &mut status);
            let sample = ManuallyDrop::take(&mut out[0].pSample);
            drop(ManuallyDrop::take(&mut out[0].pEvents));
            if let Err(e) = &result
                && e.code() == MF_E_TRANSFORM_STREAM_CHANGE
            {
                let media_type = self.mft.GetOutputAvailableType(self.output_id, 0)?;
                self.mft.SetOutputType(self.output_id, &media_type, 0)?;
                self.set(&CODECAPI_AVEncVideoMaxQP, VARIANT::from(51u32))?;
                self.set(
                    &CODECAPI_AVEncCommonMeanBitRate,
                    VARIANT::from(self.bitrate),
                )?;
                self.output_credit -= 1;
                return Ok(Vec::new());
            }
            self.output_credit -= 1;
            result.context("hardware MFT ProcessOutput")?;
            let sample = sample.context("hardware MFT returned no sample")?;
            let buffer = sample.ConvertToContiguousBuffer()?;
            let (mut data, mut length) = (std::ptr::null_mut(), 0);
            buffer.Lock(&mut data, None, Some(&mut length))?;
            let bytes = std::slice::from_raw_parts(data, length as usize).to_vec();
            buffer.Unlock()?;
            ensure!(
                bytes.starts_with(&[0, 0, 0, 1]) || bytes.starts_with(&[0, 0, 1]),
                "hardware MFT did not produce Annex B H264"
            );
            self.last_output = Instant::now();
            // Driver property acceptance is insufficient: qualify actual output.
            // Allow one second for RC to settle, then measure a second of media.
            if self.frame > self.fps as i64 {
                self.rate_bytes += bytes.len() as u64;
                self.rate_frames += 1;
                if self.rate_frames >= self.fps {
                    let measured = self.rate_bytes * 8 * self.fps as u64 / self.rate_frames as u64;
                    ensure!(
                        measured <= self.bitrate as u64 * 3 / 2,
                        "hardware H264 rate control exceeded cap: measured {measured} bps, target {} bps; driver cannot sustain this cap at {}x{}@{}",
                        self.bitrate,
                        self.width,
                        self.height,
                        self.fps
                    );
                    self.rate_bytes = 0;
                    self.rate_frames = 0;
                }
            }
            Ok(bytes)
        }
    }
}
