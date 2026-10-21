fn omnibox_gate(c: &mut Client, id: u64, desktop: &DesktopProbe) -> Result<()> {
    let fixture = InputFixture::static_popup_page()?;
    let red = format!("http://127.0.0.1:{}/red?omnibox", fixture.port);
    let blue = format!("http://127.0.0.1:{}/blue?omnibox", fixture.port);
    let mode = if std::env::var("PRTS_FRAME_MODE").as_deref() == Ok("cpu") {
        2u8
    } else {
        1
    };
    let credits = if std::env::var_os("PRTS_M5_ONE_CREDIT").is_some() {
        1u32
    } else {
        2
    };
    let faults_before = c.faults.len();
    let missing_before = c.composed.missing_layers;
    let mut config = id.to_le_bytes().to_vec();
    config.push(mode);
    config.extend(credits.to_le_bytes());
    config.extend(30u32.to_le_bytes());
    ensure!(
        c.request(WINDOW_FRAME_CONFIG, config)?.kind == WINDOW_RESULT,
        "Omnibox capture config failed"
    );
    for (width, height, dpr) in [(960, 640, 1.), (960, 640, 2.), (961, 641, 1.25)] {
        let mut resize = id.to_le_bytes().to_vec();
        Size { width, height, dpr }.encode(&mut resize)?;
        let response = c.request(WINDOW_RESIZE, resize)?;
        ensure!(response.kind == WINDOW_RESULT, "Omnibox resize failed");
        let w = Window::decode(&response.payload)?;
        let (_, _, _, rects) = c.geometry(id)?;
        let mut navigate = w.active_tab.to_le_bytes().to_vec();
        v1::string(&red, &mut navigate)?;
        ensure!(
            c.request(TAB_NAVIGATE, navigate)?.kind == v1::OK,
            "Omnibox fixture navigate failed"
        );
        c.send(&w, v1::Input::Focus(true), 0)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let (base, base_pixels) = loop {
            let frame = c.frame(id)?;
            let pixels = frame_pixels(&frame)?;
            let at = ((frame.height as usize * 3 / 4) * frame.width as usize
                + frame.width as usize / 2)
                * 4;
            if pixels[at..at + 3] == [60, 30, 220] && context_title(c, id)? == "popup-red" {
                break (frame, pixels);
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "Omnibox fixture did not paint"
            );
        };
        let preference = if mode == 1 { "qsv" } else { "software" };
        let (mut encoder, warning) =
            prtsd::encoder::Selected::new(preference, base.width, base.height, 4_000_000, 30)?;
        ensure!(warning.is_none(), "Omnibox encoder fallback: {warning:?}");
        let page_top = ((rects[3].y + 4.) * dpr).ceil() as usize;
        let witness_bottom = (page_top + (120. * dpr).ceil() as usize).min(base.height as usize);
        let opened = std::time::Instant::now();
        c.shortcut(&w, 'L')?;
        c.send(&w, v1::Input::Char(blue.clone()), 0)?;
        let (composite, changed) = loop {
            let frame = c.frame(id)?;
            let readback = frame
                .texture
                .as_ref()
                .map(|_| frame_pixels(&frame))
                .transpose()?;
            let pixels = readback.as_deref().unwrap_or(&frame.pixels);
            // Look directly below the omnibox, where suggestions must appear.
            // Borrow CPU pixels and avoid scanning the unchanged lower page.
            let changed = pixels
                [page_top * frame.width as usize * 4..witness_bottom * frame.width as usize * 4]
                .as_chunks::<4>()
                .0
                .iter()
                .zip(
                    base_pixels[page_top * frame.width as usize * 4
                        ..witness_bottom * frame.width as usize * 4]
                        .as_chunks::<4>()
                        .0
                        .iter(),
                )
                .filter(|(a, b)| a[..3].iter().zip(&b[..3]).any(|(a, b)| a.abs_diff(*b) > 50))
                // A 1001-pixel witness satisfies the visibility assertion.
                // Bound diagnostic work inside the appearance measurement.
                .take(1001)
                .count();
            if changed > 1000 {
                break (frame, changed);
            }
            ensure!(
                opened.elapsed() <= appearance_deadline(mode),
                "Omnibox dropdown appearance deadline; hosts={:?}",
                c.hosts(id)?
            );
        };
        let capture_ms = opened.elapsed().as_secs_f64() * 1000.;
        let codec_started = std::time::Instant::now();
        let rgb = decode_composite(&mut encoder, &composite)?;
        println!(
            "OMNIBOX show capture_ms={capture_ms:.2} encode_decode_ms={:.2} decoded_ms={:.2}",
            codec_started.elapsed().as_secs_f64() * 1000.,
            opened.elapsed().as_secs_f64() * 1000.
        );
        decoded_appearance(mode, opened, "Omnibox")?;
        let decoded_changed = rgb[page_top * composite.width as usize * 3
            ..witness_bottom * composite.width as usize * 3]
            .as_chunks::<3>()
            .0
            .iter()
            .zip(
                base_pixels[page_top * composite.width as usize * 4
                    ..witness_bottom * composite.width as usize * 4]
                    .as_chunks::<4>()
                    .0
                    .iter(),
            )
            .filter(|(a, b)| {
                a.iter()
                    .zip([b[2], b[1], b[0]])
                    .any(|(a, b)| a.abs_diff(b) > 50)
            })
            .count();
        ensure!(
            decoded_changed > 1000,
            "Omnibox suggestions absent in decoded video"
        );
        ensure!(
            (mode == 1) == composite.texture.is_some(),
            "Omnibox composition transport changed"
        );
        ensure!(
            mode != 1 || c.composed.missing_layers == missing_before,
            "Omnibox missing-layer diagnostics"
        );
        ensure!(
            c.resources.resource_count() <= credits as usize,
            "Omnibox resource pool exceeded"
        );
        desktop.check(c.raw.pid, "M5 streamed omnibox suggestions")?;
        c.press(&w, 27, "Escape", "Escape")?;
        c.geometry(id)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let frame = c.frame(id)?;
            let pixels = frame_pixels(&frame)?;
            if pixels[page_top * frame.width as usize * 4..]
                == base_pixels[page_top * frame.width as usize * 4..]
            {
                break;
            }
            if std::time::Instant::now() >= deadline {
                let differences: Vec<_> = pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(base_pixels.as_chunks::<4>().0.iter())
                    .enumerate()
                    .filter(|(_, (a, b))| a != b)
                    .map(|(i, _)| (i % frame.width as usize, i / frame.width as usize))
                    .collect();
                eprintln!(
                    "Omnibox restoration differences={} first={:?} hosts={:?}",
                    differences.len(),
                    differences.first(),
                    c.hosts(id)?
                );
                let _ = std::fs::write("C:/src/chromium/prts-omnibox-before.bgra", &base_pixels);
                let _ = std::fs::write("C:/src/chromium/prts-omnibox-after.bgra", &pixels);
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "Omnibox close left popup ghosts"
            );
        }
        c.shortcut(&w, 'L')?;
        let selected_url = format!("{blue}&selected={width}-{height}-{dpr}");
        c.send(&w, v1::Input::Char(selected_url.clone()), 0)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let selected = loop {
            c.hosts(id)?;
            if let Some((_, bounds)) = c
                .menu_items
                .iter()
                .find(|(marker, _)| *marker == 0x7fff_0001)
            {
                break bounds.clone();
            }
            let result = c.request(0x42b, id.to_le_bytes().to_vec())?;
            ensure!(
                result.kind == v1::OK,
                "WebUI omnibox geometry probe failed: {result:?}"
            );
            let mut data = v1::Decoder::new(&result.payload);
            let bounds = read_rect(&mut data)?;
            data.finish()?;
            if bounds.width > 0. && bounds.height > 0. {
                break bounds;
            }
            if std::time::Instant::now() >= deadline {
                c.geometry(id)?;
                let failure_hosts = c.hosts(id)?;
                eprintln!(
                    "Omnibox selection failure expected={selected_url:?} faults={:?} hosts={:?}",
                    c.faults, failure_hosts
                );
                let frame = c.frame(id)?;
                let _ = std::fs::write(
                    "C:/src/chromium/prts-omnibox-open.bgra",
                    frame_pixels(&frame)?,
                );
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "Selected omnibox row geometry missing; probe={:?}",
                c.menu_items
            );
            c.pump_for(Duration::from_millis(10))?;
        };
        ensure!(
            selected.width > 0.
                && selected.height > 0.
                && selected.x >= 0.
                && selected.y >= 0.
                && selected.x + selected.width <= f64::from(width)
                && selected.y + selected.height <= f64::from(height),
            "Omnibox selected row outside stream: {selected:?}"
        );
        c.click(
            &w,
            selected.x + selected.width / 2.,
            selected.y + selected.height / 2.,
        )?;
        c.wait_window(id, |window| {
            window
                .tabs
                .iter()
                .any(|tab| tab.active && tab.url == selected_url && tab.load == 2)
        })?;
        c.decoded_page(id, [20, 60, 210])?;
        c.wait_hosts(id, false)?;
        ensure!(
            c.faults.len() == faults_before,
            "Omnibox input faults: {:?}",
            &c.faults[faults_before..]
        );
        println!(
            "PASS decoded omnibox popup/close/pointer navigation {width}x{height}@{dpr} mode={mode} credits={credits} changed={changed}/{decoded_changed}"
        );
    }
    Ok(())
}
