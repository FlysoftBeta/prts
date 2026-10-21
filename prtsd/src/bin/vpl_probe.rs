fn main() -> anyhow::Result<()> {
    println!("{}", prtsd::vpl_encoder::probe()?);
    if std::env::args().any(|a| a == "--encode") {
        use openh264::formats::YUVSource;
        use prtsd::{encoder::VideoEncoder, vpl_encoder::VplEncoder};
        use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
        for gpu_input in [false, true] {
            let (width, height) = (640, 480);
            let gpu = prtsd::d3d::shared()?;
            let texture =
                gpu.lock()
                    .unwrap()
                    .texture(width, height, DXGI_FORMAT_B8G8R8A8_UNORM, false)?;
            let frame = prtsd::d3d::Frame { texture, gpu };
            let mut encoder = VplEncoder::new(width, height, 4_000_000, 30)?;
            let mut decoder = openh264::decoder::Decoder::new()?;
            let pixels = [20, 180, 90, 255].repeat((width * height) as usize);
            for index in 0..12 {
                if index == 6 {
                    encoder.set_bitrate(150_000)?;
                }
                let bytes = if gpu_input {
                    let g = frame.gpu.lock().unwrap();
                    unsafe {
                        g.context.UpdateSubresource(
                            &frame.texture,
                            0,
                            None,
                            pixels.as_ptr().cast(),
                            width * 4,
                            0,
                        );
                    }
                    drop(g);
                    encoder.encode_texture(&frame, index == 0 || index == 6)?
                } else {
                    encoder.encode(
                        &pixels,
                        width as usize,
                        height as usize,
                        index == 0 || index == 6,
                    )?
                };
                let mut pictures = 0;
                for nal in openh264::nal_units(&bytes) {
                    if let Some(pic) = decoder.decode(nal)? {
                        anyhow::ensure!(
                            pic.dimensions() == (width as usize, height as usize),
                            "wrong decoded dimensions"
                        );
                        pictures += 1;
                    }
                }
                anyhow::ensure!(
                    pictures == 1,
                    "oneVPL output did not immediately decode one picture"
                );
            }
            println!("PASS oneVPL gpu_input={gpu_input}:12/12 immediate decode and150kbps reset");
        }
    }
    Ok(())
}
