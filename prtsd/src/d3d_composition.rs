//! Premultiplied popup composition on the capture/encoder's Intel D3D11 device.
use crate::{composition::Placement, d3d::Device};
use anyhow::{Context, Result, ensure};
use windows::{
    Win32::Graphics::{
        Direct3D::{Fxc::*, *},
        Direct3D11::*,
        Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
    },
    core::{PCSTR, s},
};

pub struct Pipeline {
    vertex: ID3D11VertexShader,
    pixel: ID3D11PixelShader,
    sampler: ID3D11SamplerState,
    blend: ID3D11BlendState,
    raster: ID3D11RasterizerState,
    params: ID3D11Buffer,
}
fn compile(entry: PCSTR, target: PCSTR) -> Result<Vec<u8>> {
    let source = include_bytes!("composition.hlsl");
    let (mut code, mut errors) = (None, None);
    unsafe {
        let result = D3DCompile(
            source.as_ptr().cast(),
            source.len(),
            PCSTR::null(),
            None,
            None::<&ID3DInclude>,
            entry,
            target,
            D3DCOMPILE_ENABLE_STRICTNESS,
            0,
            &mut code,
            Some(&mut errors),
        );
        if let Err(error) = result {
            let diagnostic = errors
                .map(|blob| {
                    String::from_utf8_lossy(std::slice::from_raw_parts(
                        blob.GetBufferPointer().cast::<u8>(),
                        blob.GetBufferSize(),
                    ))
                    .into_owned()
                })
                .unwrap_or_default();
            anyhow::bail!("Popup shader compilation: {error}: {diagnostic}");
        }
        let code = code.context("Shader compiler returned no bytecode")?;
        Ok(
            std::slice::from_raw_parts(code.GetBufferPointer().cast::<u8>(), code.GetBufferSize())
                .to_vec(),
        )
    }
}
impl Pipeline {
    pub fn new(gpu: &Device) -> Result<Self> {
        let vertex_code = compile(s!("vs"), s!("vs_4_0"))?;
        let pixel_code = compile(s!("ps"), s!("ps_4_0"))?;
        let (mut vertex, mut pixel, mut sampler, mut blend, mut raster, mut params) =
            (None, None, None, None, None, None);
        unsafe {
            gpu.device.CreateVertexShader(
                &vertex_code,
                None::<&ID3D11ClassLinkage>,
                Some(&mut vertex),
            )?;
            gpu.device.CreatePixelShader(
                &pixel_code,
                None::<&ID3D11ClassLinkage>,
                Some(&mut pixel),
            )?;
            gpu.device.CreateSamplerState(
                &D3D11_SAMPLER_DESC {
                    Filter: D3D11_FILTER_MIN_MAG_MIP_POINT,
                    AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
                    AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
                    MaxLOD: f32::MAX,
                    ..Default::default()
                },
                Some(&mut sampler),
            )?;
            let mut desc = D3D11_BLEND_DESC::default();
            desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: true.into(),
                SrcBlend: D3D11_BLEND_ONE,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            gpu.device.CreateBlendState(&desc, Some(&mut blend))?;
            gpu.device.CreateRasterizerState(
                &D3D11_RASTERIZER_DESC {
                    FillMode: D3D11_FILL_SOLID,
                    CullMode: D3D11_CULL_NONE,
                    DepthClipEnable: true.into(),
                    ..Default::default()
                },
                Some(&mut raster),
            )?;
            gpu.device.CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: 32,
                    Usage: D3D11_USAGE_DEFAULT,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    ..Default::default()
                },
                None,
                Some(&mut params),
            )?;
        }
        Ok(Self {
            vertex: vertex.context("Missing vertex shader")?,
            pixel: pixel.context("Missing pixel shader")?,
            sampler: sampler.context("Missing sampler")?,
            blend: blend.context("Missing blend state")?,
            raster: raster.context("Missing rasterizer")?,
            params: params.context("Missing shader params")?,
        })
    }
    /// Caller serializes the shared immediate context. No staging/readback.
    pub fn compose(
        &self,
        gpu: &Device,
        base: &ID3D11Texture2D,
        layers: &[(Placement, &ID3D11Texture2D)],
    ) -> Result<ID3D11Texture2D> {
        ensure!(layers.len() <= 8, "Too many popup layers");
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe {
            base.GetDesc(&mut desc);
        }
        ensure!(
            desc.Format == DXGI_FORMAT_B8G8R8A8_UNORM
                && desc.Width > 0
                && desc.Height > 0
                && u64::from(desc.Width) * u64::from(desc.Height) <= 64_000_000,
            "Invalid composition base"
        );
        for (placement, _) in layers {
            placement.validate()?;
        }
        let output = gpu.texture(desc.Width, desc.Height, desc.Format, false)?;
        // ClearState also drops context-held RTV/SRV references after errors.
        let result = (|| -> Result<()> {
            unsafe {
                gpu.context.CopyResource(&output, base);
                let mut view = None;
                gpu.device
                    .CreateRenderTargetView(&output, None, Some(&mut view))?;
                gpu.context
                    .OMSetRenderTargets(Some(&[view]), None::<&ID3D11DepthStencilView>);
                gpu.context.OMSetBlendState(&self.blend, None, u32::MAX);
                gpu.context.RSSetState(&self.raster);
                gpu.context.IASetInputLayout(None::<&ID3D11InputLayout>);
                gpu.context
                    .IASetPrimitiveTopology(D3D_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                gpu.context.VSSetShader(&self.vertex, None);
                gpu.context.PSSetShader(&self.pixel, None);
                gpu.context
                    .PSSetSamplers(0, Some(&[Some(self.sampler.clone())]));
                gpu.context
                    .PSSetConstantBuffers(0, Some(&[Some(self.params.clone())]));
                for (p, texture) in layers {
                    let mut source_desc = D3D11_TEXTURE2D_DESC::default();
                    texture.GetDesc(&mut source_desc);
                    ensure!(
                        source_desc.Format == desc.Format
                            && source_desc.Width > 0
                            && source_desc.Height > 0,
                        "Incompatible popup texture"
                    );
                    let mut source = None;
                    gpu.device
                        .CreateShaderResourceView(*texture, None, Some(&mut source))?;
                    gpu.context.PSSetShaderResources(0, Some(&[source]));
                    let pad = p.shadow.ceil();
                    gpu.context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                        TopLeftX: p.x as f32 - pad,
                        TopLeftY: p.y as f32 - pad,
                        Width: p.width as f32 + pad * 2.,
                        Height: p.height as f32 + pad * 2.,
                        MinDepth: 0.,
                        MaxDepth: 1.,
                    }]));
                    let params = [
                        p.width as f32,
                        p.height as f32,
                        p.opacity,
                        p.shadow,
                        p.corners[0],
                        p.corners[1],
                        p.corners[2],
                        p.corners[3],
                    ];
                    gpu.context.UpdateSubresource(
                        &self.params,
                        0,
                        None,
                        params.as_ptr().cast(),
                        0,
                        0,
                    );
                    gpu.context.Draw(3, 0);
                    gpu.context.PSSetShaderResources(0, Some(&[None]));
                }
                gpu.finish()?;
            }
            Ok(())
        })();
        unsafe {
            gpu.context.ClearState();
        }
        result?;
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{composition::blend_cpu, d3d};
    #[test]
    #[ignore = "Requires Intel hardware D3D11"]
    fn gpu_matches_cpu_alpha_mask_shadow_and_close() -> Result<()> {
        let gpu = d3d::shared()?;
        let gpu = gpu.lock().unwrap();
        let pipeline = Pipeline::new(&gpu)?;
        let base_bytes = [31, 71, 151, 255].repeat(96 * 64);
        let popup_bytes = [80, 10, 40, 128].repeat(32 * 24);
        let base = gpu.texture(96, 64, DXGI_FORMAT_B8G8R8A8_UNORM, false)?;
        let popup = gpu.texture(32, 24, DXGI_FORMAT_B8G8R8A8_UNORM, false)?;
        unsafe {
            gpu.context
                .UpdateSubresource(&base, 0, None, base_bytes.as_ptr().cast(), 96 * 4, 0);
            gpu.context
                .UpdateSubresource(&popup, 0, None, popup_bytes.as_ptr().cast(), 32 * 4, 0);
        }
        for (x, y, w, h, shadow, opacity) in [
            (12, 11, 32, 24, 0., 1.),
            (1, -3, 40, 30, 12., 1.),
            (71, 40, 32, 24, 8., 0.6),
            (11, 9, 40, 30, 0., 0.5),
        ] {
            let p = Placement {
                x,
                y,
                width: w,
                height: h,
                corners: [8., 4., 12., 0.],
                opacity,
                shadow,
            };
            let cpu = blend_cpu(&base_bytes, 96, 64, &[(p, 32, 24, &popup_bytes)])?;
            let output = pipeline.compose(&gpu, &base, &[(p, &popup)])?;
            let actual = gpu.readback(&output)?;
            let max = cpu
                .iter()
                .zip(&actual)
                .map(|(a, b)| a.abs_diff(*b))
                .max()
                .unwrap();
            ensure!(
                max <= 1,
                "GPU/CPU alpha/mask/shadow mismatch max={max} placement={p:?}"
            );
        }
        let closed = pipeline.compose(&gpu, &base, &[])?;
        ensure!(
            gpu.readback(&closed)? == base_bytes,
            "Popup close left ghost pixels"
        );
        Ok(())
    }
}
