fn capture_gate(c: &mut Client, w: Window) -> Result<()> {
    use prtsd::encoder::VideoEncoder;
    let url = "data:text/html,<body style='margin:0;background:rgb(220,30,60)'><script>function report(){document.title=JSON.stringify({dpr:devicePixelRatio,w:innerWidth,h:innerHeight})}onresize=report;setInterval(report,50);report()</script>";
    let mut nav = w.active_tab.to_le_bytes().to_vec();
    v1::string(url, &mut nav)?;
    ensure!(
        c.request(TAB_NAVIGATE, nav)?.kind == v1::OK,
        "Navigate failed"
    );
    let mode = if std::env::var("PRTS_FRAME_MODE").as_deref() == Ok("cpu") {
        2u8
    } else {
        1
    };
    let mut config = w.id.to_le_bytes().to_vec();
    config.push(mode);
    config.extend(2u32.to_le_bytes());
    config.extend(30u32.to_le_bytes());
    let r = c.request(WINDOW_FRAME_CONFIG, config)?;
    ensure!(r.kind == WINDOW_RESULT, "Frame config failed {r:?}");
    let mut generation = w.generation;
    for (width, height, dsf) in [
        (960, 640, 1.),
        (960, 640, 2.),
        (960, 640, 1.25),
        (961, 641, 1.25),
        (1001, 777, 1.25),
        (1203, 803, 1.),
    ] {
        let mut resize = w.id.to_le_bytes().to_vec();
        Size {
            width,
            height,
            dpr: dsf,
        }
        .encode(&mut resize)?;
        let r = c.request(WINDOW_RESIZE, resize)?;
        ensure!(r.kind == WINDOW_RESULT, "Resize failed {r:?}");
        let current = Window::decode(&r.payload)?;
        ensure!(
            current.generation > generation,
            "Resize generation did not advance"
        );
        generation = current.generation;
        let (pw, ph, actual_dsf, rects) = c.geometry(w.id)?;
        println!(
            "GEOMETRY {width}x{height}@{dsf}: compositor={pw}x{ph}@{actual_dsf}, Views={rects:?}"
        );
        ensure!(
            pw == (f64::from(width) * dsf).ceil() as u32
                && ph == (f64::from(height) * dsf).ceil() as u32
                && (actual_dsf - dsf).abs() < 0.001,
            "Compositor geometry/DSF mismatch"
        );
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let all = c.list()?;
            let current = all.iter().find(|v| v.id == w.id).context("Window lost")?;
            let title = &current
                .tabs
                .iter()
                .find(|t| t.active)
                .context("No active tab")?
                .title;
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(title)
                && value["dpr"].as_f64() == Some(dsf)
                && value["h"]
                    .as_f64()
                    .is_some_and(|h| (h - rects[3].height).abs() <= 1.)
                && value["w"]
                    .as_f64()
                    .is_some_and(|v| (v - rects[3].width).abs() <= 1.)
            {
                ensure!(
                    value["h"].as_f64().context("Missing innerHeight")? < f64::from(height),
                    "Page viewport includes Chrome UI"
                );
                break;
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "Page DPR/viewport did not update: {title}"
            );
        }
        let (frame, source_pixels) = loop {
            let f = c.frame(w.id).context("Receiving compatible root frame")?;
            ensure!(f.width == pw && f.height == ph, "Frame dimensions mismatch");
            let pixels = if let Some(texture) = &f.texture {
                texture
                    .gpu
                    .lock()
                    .unwrap()
                    .readback(&texture.texture)
                    .context("Reading GPU frame for pixel acceptance")?
            } else {
                f.pixels.clone()
            };
            let i = ((ph as usize * 3 / 4) * pw as usize + pw as usize / 2) * 4;
            if pixels[i..i + 3]
                .iter()
                .zip([60u8, 30, 220])
                .all(|(a, b)| a.abs_diff(b) <= 2)
            {
                break (f, pixels);
            }
            ensure!(std::time::Instant::now() < deadline, "Page color missing");
        };
        let preference = "qsv";
        println!("ENCODE {pw}x{ph} preference={preference}");
        let (mut encoder, warning) =
            prtsd::encoder::Selected::new(preference, pw, ph, 4_000_000, 30)
                .context("Creating gate encoder")?;
        ensure!(warning.is_none(), "Encoder fallback {warning:?}");
        let mut decoder = openh264::decoder::Decoder::new()?;
        let mut rgb = None;
        for n in 0..12 {
            let encoded = if let Some(texture) = &frame.texture {
                encoder.encode_texture(texture, n == 0)
            } else {
                encoder.encode(&frame.pixels, pw as usize, ph as usize, n == 0)
            };
            let data = encoded.context("QSV must encode every size without fallback")?;
            for nal in openh264::nal_units(&data) {
                if let Some(pic) = decoder.decode(nal).with_context(|| {
                    format!("Decoding {pw}x{ph} frame {n} from {}", encoder.name())
                })? {
                    use openh264::formats::YUVSource;
                    let (dw, dh) = pic.dimensions();
                    ensure!(
                        (dw, dh) == (((pw + 1) & !1) as usize, ((ph + 1) & !1) as usize),
                        "Unexpected decoded aperture {dw}x{dh}"
                    );
                    let mut padded = vec![0; dw * dh * 3];
                    pic.write_rgb8(&mut padded);
                    let mut pixels = Vec::with_capacity(pw as usize * ph as usize * 3);
                    for row in padded.chunks_exact(dw * 3).take(ph as usize) {
                        pixels.extend_from_slice(&row[..pw as usize * 3]);
                    }
                    rgb = Some(pixels);
                }
            }
        }
        let rgb = rgb.context("Video decode stalled")?;
        for (x, y) in [(pw - 1, ph - 1), (pw - 1, ph * 3 / 4), (pw / 2, ph - 1)] {
            let source = ((y * pw + x) * 4) as usize;
            let decoded = ((y * pw + x) * 3) as usize;
            let expected = [
                source_pixels[source + 2],
                source_pixels[source + 1],
                source_pixels[source],
            ];
            ensure!(
                rgb[decoded..decoded + 3]
                    .iter()
                    .zip(expected)
                    .all(|(a, b)| a.abs_diff(b) <= 45),
                "Decoded edge {x},{y} differs from capture: {:?} vs {expected:?}",
                &rgb[decoded..decoded + 3]
            );
        }
        let i = ((ph as usize * 3 / 4) * pw as usize + pw as usize / 2) * 3;
        ensure!(
            rgb[i..i + 3]
                .iter()
                .zip([220u8, 30, 60])
                .all(|(a, b)| a.abs_diff(b) <= 15),
            "Decoded page ROI mismatch"
        );
        for (label, rect) in ["tab strip", "toolbar", "omnibox"].into_iter().zip(&rects) {
            let left = (rect.x * dsf).max(0.) as usize;
            let top = (rect.y * dsf).max(0.) as usize;
            let right = ((rect.x + rect.width) * dsf).min(f64::from(pw)) as usize;
            let bottom = ((rect.y + rect.height) * dsf).min(f64::from(ph)) as usize;
            let mut low = 255u8;
            let mut high = 0u8;
            for y in top..bottom {
                for x in left..right {
                    let v = rgb[(y * pw as usize + x) * 3];
                    low = low.min(v);
                    high = high.max(v);
                }
            }
            if high.saturating_sub(low) <= 40 {
                let mut source_low = 255u8;
                let mut source_high = 0u8;
                for y in top..bottom {
                    for x in left..right {
                        let v = source_pixels[(y * pw as usize + x) * 4 + 2];
                        source_low = source_low.min(v);
                        source_high = source_high.max(v);
                    }
                }
                eprintln!(
                    "Capture {label} ROI range {source_low}..{source_high}, decoded {low}..{high}"
                );
                let _ = std::fs::write("C:/src/chromium/prts-m5-failure-bgra.bin", &source_pixels);
                let _ = std::fs::write("C:/src/chromium/prts-m5-failure-rgb.bin", &rgb);
            }
            ensure!(
                high.saturating_sub(low) > 40,
                "Decoded {label} ROI has no controls/contrast {low}..{high}"
            );
        }
        println!(
            "PASS decoded tab-strip/toolbar/omnibox/page ROIs {width}x{height}@{dsf}; {} / transport={mode}",
            encoder.name()
        );
        for _ in 0..3 {
            ensure!(
                c.request(WINDOW_FRAME_REFRESH, w.id.to_le_bytes().to_vec())?
                    .kind
                    == v1::OK,
                "Refresh failed"
            );
            c.frame(w.id)?;
        }
        ensure!(
            c.resources.resource_count() <= 2,
            "Resize resource drain exceeded pool"
        );
    }
    println!(
        "PASS M5 step2 full UI capture, DSF1/2/1.25, odd dimensions, generation/drain and static refresh"
    );
    Ok(())
}
