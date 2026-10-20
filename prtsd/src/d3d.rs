//! Hardware-only D3D11 capture import, private copies and encoder conversion.
use anyhow::{Context, Result, ensure};
use std::{
    mem::ManuallyDrop,
    sync::{Arc, Mutex, OnceLock},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HANDLE, HMODULE},
        Graphics::{
            Direct3D::D3D_DRIVER_TYPE_UNKNOWN,
            Direct3D11::*,
            Dxgi::{Common::*, *},
        },
    },
    core::Interface,
};

pub struct Device {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub luid: u64,
}
#[derive(Clone)]
pub struct Frame {
    pub texture: ID3D11Texture2D,
    pub gpu: Arc<Mutex<Device>>,
}
static DEVICE: OnceLock<std::result::Result<Arc<Mutex<Device>>, String>> = OnceLock::new();
pub fn shared() -> Result<Arc<Mutex<Device>>> {
    DEVICE
        .get_or_init(|| {
            Device::new()
                .map(|d| Arc::new(Mutex::new(d)))
                .map_err(|e| format!("{e:#}"))
        })
        .clone()
        .map_err(anyhow::Error::msg)
}
impl Device {
    fn new() -> Result<Self> {
        unsafe {
            let factory: IDXGIFactory1 = CreateDXGIFactory1()?;
            for i in 0..32 {
                let Ok(adapter) = factory.EnumAdapters1(i) else {
                    break;
                };
                let desc = adapter.GetDesc1()?;
                if desc.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 != 0 {
                    continue;
                }
                // QSV is qualified on Intel; do not silently select another vendor.
                if desc.VendorId != 0x8086 {
                    continue;
                }
                let (mut device, mut context) = (None, None);
                D3D11CreateDevice(
                    &adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    Some(&mut context),
                )?;
                let device = device.context("D3D device missing")?;
                let context = context.context("D3D context missing")?;
                let _ = context
                    .cast::<ID3D11Multithread>()?
                    .SetMultithreadProtected(true);
                let luid = ((desc.AdapterLuid.HighPart as u32 as u64) << 32)
                    | desc.AdapterLuid.LowPart as u64;
                eprintln!("prtsd: hardware D3D11 adapter Intel luid={luid:#018x}");
                return Ok(Self {
                    device,
                    context,
                    luid,
                });
            }
            anyhow::bail!("No Intel hardware D3D11 adapter")
        }
    }
    pub fn texture(
        &self,
        w: u32,
        h: u32,
        format: DXGI_FORMAT,
        staging: bool,
    ) -> Result<ID3D11Texture2D> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: if staging {
                D3D11_USAGE_STAGING
            } else {
                D3D11_USAGE_DEFAULT
            },
            BindFlags: if staging {
                0
            } else {
                D3D11_BIND_RENDER_TARGET.0 as u32 | D3D11_BIND_SHADER_RESOURCE.0 as u32
            },
            CPUAccessFlags: if staging {
                D3D11_CPU_ACCESS_READ.0 as u32
            } else {
                0
            },
            ..Default::default()
        };
        let mut texture = None;
        unsafe {
            self.device
                .CreateTexture2D(&desc, None, Some(&mut texture))?;
        }
        texture.context("CreateTexture2D returned no texture")
    }
    pub fn open(&self, handle: HANDLE, w: u32, h: u32, luid: u64) -> Result<ID3D11Texture2D> {
        ensure!(
            luid == self.luid && luid != 0,
            "Texture adapter LUID mismatch"
        );
        unsafe {
            let texture: ID3D11Texture2D = self
                .device
                .cast::<ID3D11Device1>()?
                .OpenSharedResource1(handle)?;
            let mut d = D3D11_TEXTURE2D_DESC::default();
            texture.GetDesc(&mut d);
            ensure!(
                d.Width == w
                    && d.Height == h
                    && d.MipLevels == 1
                    && d.ArraySize == 1
                    && d.Format == DXGI_FORMAT_B8G8R8A8_UNORM
                    && d.SampleDesc.Count == 1
                    && d.SampleDesc.Quality == 0
                    && d.Usage == D3D11_USAGE_DEFAULT
                    && d.CPUAccessFlags == 0
                    && d.MiscFlags
                        == (D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0
                            | D3D11_RESOURCE_MISC_SHARED_KEYEDMUTEX.0)
                            as u32,
                "Shared texture descriptor mismatch"
            );
            texture.cast::<IDXGIKeyedMutex>()?;
            Ok(texture)
        }
    }
    pub fn finish(&self) -> Result<()> {
        unsafe {
            let mut query = None;
            self.device.CreateQuery(
                &D3D11_QUERY_DESC {
                    Query: D3D11_QUERY_EVENT,
                    MiscFlags: 0,
                },
                Some(&mut query),
            )?;
            let query = query.context("Missing GPU query")?;
            self.context.End(&query);
            self.context.Flush();
            let end = Instant::now() + Duration::from_secs(1);
            loop {
                // The generated Result wrapper discards S_FALSE. Inspect HRESULT.
                let hr = (Interface::vtable(&self.context).GetData)(
                    Interface::as_raw(&self.context),
                    Interface::as_raw(&query),
                    std::ptr::null_mut(),
                    0,
                    0,
                );
                if hr.0 == 0 {
                    return Ok(());
                }
                hr.ok()?;
                ensure!(Instant::now() < end, "D3D GPU completion timeout");
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }
    pub fn copy_shared(
        &self,
        texture: &ID3D11Texture2D,
        w: u32,
        h: u32,
    ) -> Result<ID3D11Texture2D> {
        let output = self.texture(w, h, DXGI_FORMAT_B8G8R8A8_UNORM, false)?;
        unsafe {
            let mutex = texture.cast::<IDXGIKeyedMutex>()?;
            let hr = (Interface::vtable(&mutex).AcquireSync)(Interface::as_raw(&mutex), 1, 1000);
            ensure!(
                hr.0 == 0,
                "Keyed mutex AcquireSync(1) failed: {hr:?}; restart required"
            );
            self.context.CopyResource(&output, texture);
            // On failure never forge a key transition/ACK. The connection owns teardown.
            self.finish()?;
            mutex.ReleaseSync(0)?;
        }
        Ok(output)
    }
    pub fn readback(&self, texture: &ID3D11Texture2D) -> Result<Vec<u8>> {
        unsafe {
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            texture.GetDesc(&mut desc);
            let staging = self.texture(desc.Width, desc.Height, desc.Format, true)?;
            self.context.CopyResource(&staging, texture);
            let mut map = D3D11_MAPPED_SUBRESOURCE::default();
            self.context
                .Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut map))?;
            let row = desc.Width as usize * 4;
            let mut bytes = vec![0; row * desc.Height as usize];
            for (y, dst) in bytes.chunks_exact_mut(row).enumerate() {
                dst.copy_from_slice(std::slice::from_raw_parts(
                    (map.pData as *const u8).add(y * map.RowPitch as usize),
                    row,
                ));
            }
            self.context.Unmap(&staging, 0);
            Ok(bytes)
        }
    }
    pub fn nv12(&self, texture: &ID3D11Texture2D, w: u32, h: u32) -> Result<ID3D11Texture2D> {
        unsafe {
            let video = self.device.cast::<ID3D11VideoDevice>()?;
            let ctx = self.context.cast::<ID3D11VideoContext>()?;
            let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL {
                    Numerator: 30,
                    Denominator: 1,
                },
                InputWidth: w,
                InputHeight: h,
                OutputWidth: w,
                OutputHeight: h,
                OutputFrameRate: DXGI_RATIONAL {
                    Numerator: 30,
                    Denominator: 1,
                },
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };
            let enumerator = video.CreateVideoProcessorEnumerator(&desc)?;
            let processor = video.CreateVideoProcessor(&enumerator, 0)?;
            let output = self.texture(w, h, DXGI_FORMAT_NV12, false)?;
            let (mut input_view, mut output_view) = (None, None);
            video.CreateVideoProcessorInputView(
                texture,
                &enumerator,
                &D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                    ..Default::default()
                },
                Some(&mut input_view),
            )?;
            video.CreateVideoProcessorOutputView(
                &output,
                &enumerator,
                &D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                    ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                    ..Default::default()
                },
                Some(&mut output_view),
            )?;
            // RGB full range sRGB -> BT.601 limited range NV12, matching CPU conversion.
            ctx.VideoProcessorSetStreamColorSpace(
                &processor,
                0,
                &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 0 },
            );
            ctx.VideoProcessorSetOutputColorSpace(
                &processor,
                &D3D11_VIDEO_PROCESSOR_COLOR_SPACE { _bitfield: 1 << 4 },
            );
            ctx.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: true.into(),
                pInputSurface: ManuallyDrop::new(input_view),
                ..Default::default()
            };
            let result = ctx.VideoProcessorBlt(
                &processor,
                output_view.as_ref().context("Missing VP output")?,
                0,
                std::slice::from_ref(&stream),
            );
            ManuallyDrop::drop(&mut stream.pInputSurface);
            result?;
            self.finish()?;
            Ok(output)
        }
    }
}
