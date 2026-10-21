use openh264::{
    OpenH264API,
    encoder::{
        BitRate, Complexity, Encoder, EncoderConfig, FrameRate, IntraFramePeriod, Profile,
        RateControlMode, UsageType,
    },
    formats::{BgraSliceU8, YUVBuffer},
};

pub trait VideoEncoder {
    fn encode(
        &mut self,
        bgra: &[u8],
        width: usize,
        height: usize,
        keyframe: bool,
    ) -> anyhow::Result<Vec<u8>>;
}

/// CPU/GPU frame adapter with hardware selection and sticky software recovery.
pub struct Selected {
    backend: Backend,
    bps: u32,
    fps: u32,
}
enum Backend {
    Software(Box<Software>),
    #[cfg(windows)]
    Hardware(crate::mf_encoder::MfEncoder),
}
impl Selected {
    pub fn new(
        preference: &str,
        w: u32,
        h: u32,
        bps: u32,
        fps: u32,
    ) -> anyhow::Result<(Self, Option<String>)> {
        let mut warning = None;
        #[cfg(windows)]
        if preference != "software" {
            match crate::mf_encoder::MfEncoder::new(w, h, bps, fps) {
                Ok(hw) => {
                    return Ok((
                        Self {
                            backend: Backend::Hardware(hw),
                            bps,
                            fps,
                        },
                        None,
                    ));
                }
                Err(e) => {
                    warning = Some(format!(
                        "{preference} hardware encoder initialization failed: {e:#}; falling back to software"
                    ))
                }
            }
        }
        #[cfg(not(windows))]
        if preference != "software" {
            let _ = (w, h);
            warning = Some("Media Foundation requires Windows; falling back to software".into());
        }
        Ok((
            Self {
                backend: Backend::Software(Box::new(Software::new(bps, fps)?)),
                bps,
                fps,
            },
            warning,
        ))
    }
    #[cfg(windows)]
    pub fn encode_texture(
        &mut self,
        frame: &crate::d3d::Frame,
        key: bool,
    ) -> anyhow::Result<Vec<u8>> {
        match &mut self.backend {
            Backend::Hardware(hw) => hw.encode_texture(frame, key),
            Backend::Software(sw) => {
                let gpu = frame.gpu.lock().unwrap();
                let pixels = gpu.readback(&frame.texture)?;
                let mut d = windows::Win32::Graphics::Direct3D11::D3D11_TEXTURE2D_DESC::default();
                unsafe {
                    frame.texture.GetDesc(&mut d);
                }
                sw.encode(&pixels, d.Width as usize, d.Height as usize, key)
            }
        }
    }
    pub fn name(&self) -> &str {
        match &self.backend {
            Backend::Software(_) => "software/openh264",
            #[cfg(windows)]
            Backend::Hardware(hw) => hw.name(),
        }
    }
    pub fn set_bitrate(&mut self, bps: u32) -> anyhow::Result<()> {
        match &mut self.backend {
            Backend::Software(sw) => sw.set_bitrate(bps, self.fps)?,
            #[cfg(windows)]
            Backend::Hardware(hw) => hw.set_bitrate(bps)?,
        }
        self.bps = bps;
        Ok(())
    }
    pub fn fallback(&mut self) -> anyhow::Result<()> {
        self.backend = Backend::Software(Box::new(Software::new(self.bps, self.fps)?));
        Ok(())
    }
}
impl VideoEncoder for Selected {
    fn encode(&mut self, bgra: &[u8], w: usize, h: usize, key: bool) -> anyhow::Result<Vec<u8>> {
        match &mut self.backend {
            Backend::Software(sw) => sw.encode(bgra, w, h, key),
            #[cfg(windows)]
            Backend::Hardware(hw) => hw.encode(bgra, w, h, key),
        }
    }
}
pub struct Software {
    encoder: Encoder,
    started: bool,
}
impl Software {
    pub fn new(bps: u32, fps: u32) -> anyhow::Result<Self> {
        let threads =
            std::thread::available_parallelism().map_or(2, |count| count.get().min(4)) as u16;
        let config = EncoderConfig::new()
            .bitrate(BitRate::from_bps(bps))
            .max_frame_rate(FrameRate::from_hz(fps as f32))
            .usage_type(UsageType::CameraVideoRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .profile(Profile::Baseline)
            .complexity(Complexity::Low)
            // Preserve motion at useful interactive rates. Let RC raise QP first;
            // only allow frame skipping below the supported 150 kbps floor.
            .skip_frames(bps < 150_000)
            .intra_frame_period(IntraFramePeriod::from_num_frames(fps * 2))
            .num_threads(threads);
        Ok(Self {
            encoder: Encoder::with_api_config(OpenH264API::from_source(), config)?,
            started: false,
        })
    }
    pub fn set_bitrate(&mut self, bps: u32, fps: u32) -> anyhow::Result<()> {
        if !self.started {
            *self = Self::new(bps, fps)?;
            return Ok(());
        }
        let mut bitrate = openh264_sys2::SBitrateInfo {
            iLayer: openh264_sys2::SPATIAL_LAYER_ALL,
            iBitrate: bps as i32,
        };
        let mut skip = bps < 150_000;
        // SetOption borrows these correctly typed values only for the call.
        // Preserve prediction and RC history rather than emitting an IDR on
        // every REMB fluctuation.
        unsafe {
            let raw = self.encoder.raw_api();
            let code = raw.set_option(
                openh264_sys2::ENCODER_OPTION_BITRATE,
                (&raw mut bitrate).cast(),
            );
            anyhow::ensure!(code == 0, "OpenH264 bitrate update failed: {code}");
            let code = raw.set_option(
                openh264_sys2::ENCODER_OPTION_RC_FRAME_SKIP,
                (&raw mut skip).cast(),
            );
            anyhow::ensure!(code == 0, "OpenH264 frame-skip update failed: {code}");
        }
        Ok(())
    }
}
impl VideoEncoder for Software {
    fn encode(&mut self, bgra: &[u8], w: usize, h: usize, key: bool) -> anyhow::Result<Vec<u8>> {
        if key {
            self.encoder.force_intra_frame();
        }
        let yuv = bgra_to_i420(bgra, w, h);
        let result = self.encoder.encode(&yuv)?.to_vec();
        self.started = true;
        Ok(result)
    }
}
/// BT.601 limited range. Edge replication pads odd device dimensions to even I420.
pub fn bgra_to_i420(bgra: &[u8], w: usize, h: usize) -> YUVBuffer {
    if w.is_multiple_of(2) && h.is_multiple_of(2) {
        // OpenH264's packed-BGRA converter selects AVX2/SIMD when available.
        // Keep explicit edge replication for odd-sized capture below.
        return YUVBuffer::from_bgra8_source(BgraSliceU8::new(bgra, (w, h)));
    }
    let ew = (w + 1) & !1;
    let eh = (h + 1) & !1;
    bgra_to_i420_padded(bgra, w, h, ew, eh)
}
pub(crate) fn bgra_to_i420_padded(
    bgra: &[u8],
    w: usize,
    h: usize,
    ew: usize,
    eh: usize,
) -> YUVBuffer {
    let size = ew * eh;
    let mut data = vec![0; size * 3 / 2];
    let clamp = |v: i32| v.clamp(0, 255) as u8;
    for y in 0..eh {
        for x in 0..ew {
            let i = (y.min(h - 1) * w + x.min(w - 1)) * 4;
            let (b, g, r) = (bgra[i] as i32, bgra[i + 1] as i32, bgra[i + 2] as i32);
            data[y * ew + x] = clamp(((66 * r + 129 * g + 25 * b + 128) >> 8) + 16);
        }
    }
    for y in (0..eh).step_by(2) {
        for x in (0..ew).step_by(2) {
            let (mut b, mut g, mut r) = (0, 0, 0);
            for dy in 0..2 {
                for dx in 0..2 {
                    let i = ((y + dy).min(h - 1) * w + (x + dx).min(w - 1)) * 4;
                    b += bgra[i] as i32;
                    g += bgra[i + 1] as i32;
                    r += bgra[i + 2] as i32;
                }
            }
            b /= 4;
            g /= 4;
            r /= 4;
            let i = y / 2 * (ew / 2) + x / 2;
            data[size + i] = clamp(((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128);
            data[size + size / 4 + i] = clamp(((112 * r - 94 * g - 18 * b + 128) >> 8) + 128);
        }
    }
    YUVBuffer::from_vec(data, ew, eh)
}

#[cfg(test)]
mod conversion_tests {
    use super::*;
    use openh264::formats::YUVSource;

    #[test]
    fn packed_conversion_preserves_limited_range_and_chroma() {
        // Exercise SIMD blocks and scalar tails with arbitrary alpha/chroma.
        for (w, h) in [(2, 2), (6, 4), (34, 10), (80, 38)] {
            let mut state = 0x463ab251u32;
            let pixels: Vec<_> = (0..w * h * 4)
                .map(|_| {
                    state ^= state << 13;
                    state ^= state >> 17;
                    state ^= state << 5;
                    state as u8
                })
                .collect();
            let actual = bgra_to_i420(&pixels, w, h);
            let reference = bgra_to_i420_padded(&pixels, w, h, w, h);
            assert!(
                actual
                    .y()
                    .iter()
                    .zip(reference.y())
                    .all(|(a, b)| a.abs_diff(*b) <= 1)
            );
            // The packed converter rounds 2x2 chroma averages; the original
            // scalar path truncates them. Both use BT.601 limited coefficients.
            for (actual, reference) in [(actual.u(), reference.u()), (actual.v(), reference.v())] {
                assert!(
                    actual
                        .iter()
                        .zip(reference)
                        .all(|(a, b)| a.abs_diff(*b) <= 2)
                );
            }
        }
        for (bgra, expected) in [([0, 0, 0, 255], 16), ([255, 255, 255, 255], 235)] {
            let actual = bgra_to_i420(&bgra.repeat(4), 2, 2);
            assert!(actual.y().iter().all(|v| *v == expected));
            assert_eq!(actual.u(), &[128]);
            assert_eq!(actual.v(), &[128]);
        }
    }
}
