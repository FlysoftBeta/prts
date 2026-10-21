//! Intel oneVPL H.264 with synchronous D3D11 submissions and checked settings.
use anyhow::{Result, ensure};
use std::{
    ffi::{CStr, c_char, c_void},
    ptr::NonNull,
    sync::{Arc, Mutex},
};
use windows::Win32::Graphics::{
    Direct3D11::ID3D11Texture2D, Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
};
use windows::core::Interface;

unsafe extern "C" {
    fn prts_vpl_probe(device: *mut c_void, output: *mut c_char, capacity: u32) -> i32;
    fn prts_vpl_create(
        device: *mut c_void,
        width: u32,
        height: u32,
        bps: u32,
        fps: u32,
        intra_refresh: u32,
        output: *mut *mut c_void,
        error: *mut c_char,
        capacity: u32,
    ) -> i32;
    fn prts_vpl_destroy(encoder: *mut c_void);
    fn prts_vpl_bitrate(encoder: *mut c_void, bps: u32, error: *mut c_char, capacity: u32) -> i32;
    fn prts_vpl_encode(
        encoder: *mut c_void,
        texture: *mut c_void,
        key: u32,
        data: *mut u8,
        size: u32,
        written: *mut u32,
        error: *mut c_char,
        capacity: u32,
    ) -> i32;
}

fn check(status: i32, error: &[c_char; 1024]) -> Result<()> {
    // Every bridge call NUL-terminates diagnostics within this initialized buffer.
    let message = unsafe { CStr::from_ptr(error.as_ptr()) }.to_string_lossy();
    ensure!(status == 0, "oneVPL: {message}");
    Ok(())
}

pub struct VplEncoder {
    native: NonNull<c_void>,
    gpu: Arc<Mutex<crate::d3d::Device>>,
    device: *mut c_void,
    width: u32,
    height: u32,
    upload: Option<ID3D11Texture2D>,
    bitstream: Vec<u8>,
}
impl VplEncoder {
    pub fn new(width: u32, height: u32, bps: u32, fps: u32) -> Result<Self> {
        Self::new_with_intra_refresh(width, height, bps, fps, false)
    }
    pub fn new_with_intra_refresh(
        width: u32,
        height: u32,
        bps: u32,
        fps: u32,
        intra_refresh: bool,
    ) -> Result<Self> {
        ensure!(
            width > 0
                && height > 0
                && width <= 4096
                && height <= 4096
                && u64::from(width) * u64::from(height) <= 8_294_400
                && fps > 0
                && fps <= 60
                && (1000..=50_000_000).contains(&bps),
            "invalid oneVPL configuration"
        );
        let gpu = crate::d3d::shared()?;
        let device = gpu.lock().unwrap().device.as_raw();
        let mut output = std::ptr::null_mut();
        let mut error = [0 as c_char; 1024];
        let status = unsafe {
            prts_vpl_create(
                device,
                width,
                height,
                bps,
                fps,
                u32::from(intra_refresh),
                &mut output,
                error.as_mut_ptr(),
                error.len() as u32,
            )
        };
        check(status, &error)?;
        Ok(Self {
            native: NonNull::new(output)
                .ok_or_else(|| anyhow::anyhow!("oneVPL returned no encoder"))?,
            gpu,
            device,
            width,
            height,
            upload: None,
            bitstream: vec![0; (width as usize * height as usize * 4).max(2 * 1024 * 1024)],
        })
    }
    pub fn name(&self) -> &str {
        "qsv/Intel Quick Sync H.264 oneVPL"
    }
    pub fn set_bitrate(&mut self, bps: u32) -> Result<()> {
        let mut error = [0 as c_char; 1024];
        let status = unsafe {
            prts_vpl_bitrate(
                self.native.as_ptr(),
                bps,
                error.as_mut_ptr(),
                error.len() as u32,
            )
        };
        check(status, &error)
    }
    fn encode_nv12(&mut self, texture: &ID3D11Texture2D, key: bool) -> Result<Vec<u8>> {
        let mut error = [0 as c_char; 1024];
        let mut written = 0;
        let status = unsafe {
            prts_vpl_encode(
                self.native.as_ptr(),
                texture.as_raw(),
                u32::from(key),
                self.bitstream.as_mut_ptr(),
                self.bitstream.len() as u32,
                &mut written,
                error.as_mut_ptr(),
                error.len() as u32,
            )
        };
        check(status, &error)?;
        ensure!(
            written > 0 && written as usize <= self.bitstream.len(),
            "invalid oneVPL output length"
        );
        Ok(self.bitstream[..written as usize].to_vec())
    }
    pub fn encode_texture(&mut self, frame: &crate::d3d::Frame, key: bool) -> Result<Vec<u8>> {
        let nv12 = {
            let gpu = frame.gpu.lock().unwrap();
            ensure!(
                gpu.device.as_raw() == self.device,
                "oneVPL D3D11 device mismatch"
            );
            gpu.nv12_aligned(&frame.texture, self.width, self.height, 16)?
        };
        self.encode_nv12(&nv12, key)
    }
}
impl crate::encoder::VideoEncoder for VplEncoder {
    fn encode(&mut self, bgra: &[u8], width: usize, height: usize, key: bool) -> Result<Vec<u8>> {
        ensure!(
            width == self.width as usize
                && height == self.height as usize
                && bgra.len() == width * height * 4,
            "oneVPL CPU input dimensions/length mismatch"
        );
        let nv12 = {
            let gpu = self.gpu.lock().unwrap();
            if self.upload.is_none() {
                self.upload = Some(gpu.texture(
                    self.width,
                    self.height,
                    DXGI_FORMAT_B8G8R8A8_UNORM,
                    false,
                )?);
            }
            let texture = self.upload.as_ref().unwrap();
            unsafe {
                gpu.context.UpdateSubresource(
                    texture,
                    0,
                    None,
                    bgra.as_ptr().cast(),
                    self.width * 4,
                    0,
                );
            }
            gpu.nv12_aligned(texture, self.width, self.height, 16)?
        };
        self.encode_nv12(&nv12, key)
    }
}
impl Drop for VplEncoder {
    fn drop(&mut self) {
        // Native ownership is unique; all synchronous submissions have finished.
        unsafe {
            prts_vpl_destroy(self.native.as_ptr());
        }
    }
}

pub fn probe() -> Result<String> {
    let gpu = crate::d3d::shared()?;
    let device = gpu.lock().unwrap().device.clone();
    let mut output = [0 as c_char; 1024];
    let status =
        unsafe { prts_vpl_probe(device.as_raw(), output.as_mut_ptr(), output.len() as u32) };
    // The bridge always terminates output when capacity is nonzero.
    let text = unsafe { CStr::from_ptr(output.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    ensure!(status == 0, "oneVPL probe failed: {text}");
    Ok(text)
}
