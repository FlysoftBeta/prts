use openh264::formats::YUVSource;
use prtsd::encoder::{Software, VideoEncoder};

const SIZES: [(u32, u32); 9] = [
    (1200, 800),
    (1202, 802),
    (1202, 800),
    (1200, 802),
    (1204, 804),
    (1203, 803),
    (1252, 972),
    (961, 641),
    (1253, 973),
];

fn pixels(w: u32, h: u32) -> Vec<u8> {
    let mut pixels = [40, 90, 180, 255].repeat((w * h) as usize);
    // Single-pixel bright right/bottom edges detect truncation and stretching.
    for y in 0..h {
        for x in 0..w {
            if x == w - 1 || y == h - 1 {
                pixels[((y * w + x) * 4) as usize..][..4].copy_from_slice(&[255; 4]);
            }
        }
    }
    pixels
}

fn check_decode(
    w: u32,
    h: u32,
    mut encode: impl FnMut(bool) -> anyhow::Result<Vec<u8>>,
) -> anyhow::Result<()> {
    let mut decoder = openh264::decoder::Decoder::new()?;
    let mut decoded = 0;
    for i in 0..12 {
        for nal in openh264::nal_units(&encode(i == 0)?) {
            if let Some(pic) = decoder.decode(nal)? {
                assert_eq!(
                    pic.dimensions(),
                    (((w + 1) & !1) as usize, ((h + 1) & !1) as usize)
                );
                let stride = pic.strides().0;
                for (x, y) in [(w - 1, h / 2), (w / 2, h - 1), (w - 1, h - 1)] {
                    let value = pic.y()[y as usize * stride + x as usize];
                    assert!(
                        value.abs_diff(235) <= 20,
                        "edge lost at {x},{y} in {w}x{h}: Y={value}"
                    );
                }
                let (dw, dh) = pic.dimensions();
                assert!(pic.y()[(dh - 1) * stride + dw - 1].abs_diff(235) <= 20);
                decoded += 1;
            }
        }
    }
    assert!(decoded >= 10, "decode stalled at {w}x{h}: {decoded}");
    eprintln!("PASS {w}x{h}: {decoded}/12 decoded, even aperture and replicated edges");
    Ok(())
}

#[test]
fn software_odd_sizes_preserve_edges() -> anyhow::Result<()> {
    for (w, h) in SIZES {
        let mut encoder = Software::new(4_000_000, 30)?;
        let pixels = pixels(w, h);
        check_decode(w, h, |key| {
            encoder.encode(&pixels, w as usize, h as usize, key)
        })?;
    }
    Ok(())
}

#[cfg(windows)]
#[test]
#[ignore = "requires Intel QSV; run --ignored --nocapture"]
fn hardware_fractional_and_odd_sizes() -> anyhow::Result<()> {
    use prtsd::{d3d, mf_encoder::MfEncoder};
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
    for (w, h) in SIZES {
        for gpu_input in [false, true] {
            let mut encoder = MfEncoder::new(w, h, 4_000_000, 30)?;
            let pixels = pixels(w, h);
            let gpu = d3d::shared()?;
            let texture = {
                let g = gpu.lock().unwrap();
                let t = g.texture(w, h, DXGI_FORMAT_B8G8R8A8_UNORM, false)?;
                unsafe {
                    g.context
                        .UpdateSubresource(&t, 0, None, pixels.as_ptr().cast(), w * 4, 0);
                }
                t
            };
            let frame = d3d::Frame { texture, gpu };
            eprintln!("QSV {} gpu={gpu_input}", encoder.name());
            check_decode(w, h, |key| {
                if gpu_input {
                    encoder.encode_texture(&frame, key)
                } else {
                    encoder.encode(&pixels, w as usize, h as usize, key)
                }
            })?;
        }
    }
    Ok(())
}

#[cfg(windows)]
#[test]
#[ignore = "requires Intel oneVPL hardware; run --release --ignored --nocapture"]
fn onevpl_fractional_and_odd_sizes() -> anyhow::Result<()> {
    use prtsd::{d3d, vpl_encoder::VplEncoder};
    use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
    for (w, h) in SIZES {
        for gpu_input in [false, true] {
            let mut encoder = VplEncoder::new(w, h, 4_000_000, 30)?;
            let pixels = pixels(w, h);
            let gpu = d3d::shared()?;
            let texture = {
                let g = gpu.lock().unwrap();
                let texture = g.texture(w, h, DXGI_FORMAT_B8G8R8A8_UNORM, false)?;
                unsafe {
                    g.context.UpdateSubresource(
                        &texture,
                        0,
                        None,
                        pixels.as_ptr().cast(),
                        w * 4,
                        0,
                    );
                }
                texture
            };
            let frame = d3d::Frame { texture, gpu };
            eprintln!("oneVPL {w}x{h} gpu={gpu_input}");
            check_decode(w, h, |key| {
                if gpu_input {
                    encoder.encode_texture(&frame, key)
                } else {
                    encoder.encode(&pixels, w as usize, h as usize, key)
                }
            })?;
        }
    }
    Ok(())
}
