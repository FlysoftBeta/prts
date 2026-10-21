fn context_click(c: &mut Client, w: &Window, x: f64, y: f64) -> Result<()> {
    c.send(w, v1::Input::MouseMove { x, y, buttons: 0 }, 0)?;
    c.send(
        w,
        v1::Input::MouseDown {
            x,
            y,
            button: 3,
            buttons: 2,
            click_count: 1,
        },
        0,
    )?;
    c.send(
        w,
        v1::Input::MouseUp {
            x,
            y,
            button: 3,
            buttons: 0,
            click_count: 1,
        },
        0,
    )
}
fn context_title(c: &mut Client, id: u64) -> Result<String> {
    let window = c
        .list()?
        .into_iter()
        .find(|w| w.id == id)
        .context("Context owner lost")?;
    Ok(window
        .tabs
        .iter()
        .find(|t| t.active)
        .context("Context active tab lost")?
        .title
        .clone())
}
fn context_gate(c: &mut Client, id: u64, desktop: &DesktopProbe) -> Result<()> {
    let faults_before = c.faults.len();
    let missing_before = c.composed.missing_layers;
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
    let w = c
        .list()?
        .into_iter()
        .find(|w| w.id == id)
        .context("Missing context owner")?;
    let mut navigate = w.active_tab.to_le_bytes().to_vec();
    v1::string(
        "data:text/html,<body style='margin:0;background:rgb(220,30,60)'><script>document.title='context-'+Math.random()</script>",
        &mut navigate,
    )?;
    ensure!(
        c.request(TAB_NAVIGATE, navigate)?.kind == v1::OK,
        "Context navigation failed"
    );
    let mut config = id.to_le_bytes().to_vec();
    config.push(mode);
    config.extend(credits.to_le_bytes());
    config.extend(30u32.to_le_bytes());
    ensure!(
        c.request(WINDOW_FRAME_CONFIG, config)?.kind == WINDOW_RESULT,
        "Context capture config failed"
    );
    for (width, height, dpr) in [(960, 640, 1.), (960, 640, 2.), (961, 641, 1.25)] {
        let mut resize = id.to_le_bytes().to_vec();
        Size { width, height, dpr }.encode(&mut resize)?;
        let response = c.request(WINDOW_RESIZE, resize)?;
        ensure!(response.kind == WINDOW_RESULT, "Context resize failed");
        let w = Window::decode(&response.payload)?;
        let (_, _, _, rects) = c.geometry(id)?;
        c.send(&w, v1::Input::Focus(true), 0)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let (base, base_pixels) = loop {
            let frame = c.frame(id)?;
            let pixels = frame_pixels(&frame)?;
            let at = ((frame.height as usize * 3 / 4) * frame.width as usize
                + frame.width as usize / 2)
                * 4;
            if pixels[at..at + 3] == [60, 30, 220] && context_title(c, id)?.starts_with("context-")
            {
                break (frame, pixels);
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "Context fixture did not paint/load"
            );
        };
        let preference = if mode == 1 { "qsv" } else { "software" };
        let (mut encoder, warning) =
            prtsd::encoder::Selected::new(preference, base.width, base.height, 4_000_000, 30)?;
        ensure!(warning.is_none(), "Context encoder fallback: {warning:?}");
        c.wait_hosts(id, false)?;
        let x = rects[3].x + 140.;
        let y = rects[3].y + 100.;
        let opened = std::time::Instant::now();
        context_click(c, &w, x, y)?;
        let page_top = (160. * dpr) as usize;
        let (composite, layer, changed) = loop {
            let frame = c.frame(id)?;
            let popup = c
                .scenes
                .get(&id)
                .and_then(|s| s.layers.iter().find(|l| l.role != 1))
                .cloned();
            if let Some(layer) = popup
                && c.composed.cached_frame(id, layer.source).is_some()
            {
                let pixels = frame_pixels(&frame)?;
                let changed = pixels[page_top * frame.width as usize * 4..]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .zip(
                        base_pixels[page_top * frame.width as usize * 4..]
                            .as_chunks::<4>()
                            .0
                            .iter(),
                    )
                    .filter(|(a, b)| a[..3].iter().zip(&b[..3]).any(|(a, b)| a.abs_diff(*b) > 50))
                    .count();
                if changed > 1000 {
                    break (frame, layer, changed);
                }
            }
            ensure!(
                opened.elapsed() <= appearance_deadline(mode),
                "Context popup appearance deadline"
            );
        };
        ensure!(
            (mode == 1) == composite.texture.is_some(),
            "Context composition transport changed"
        );
        let rgb = decode_composite(&mut encoder, &composite)?;
        decoded_appearance(mode, opened, "Context menu")?;
        let decoded_changed = rgb[page_top * composite.width as usize * 3..]
            .as_chunks::<3>()
            .0
            .iter()
            .zip(
                base_pixels[page_top * composite.width as usize * 4..]
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
            "Context absent from decoded video: {decoded_changed}"
        );
        let source = c
            .composed
            .cached_frame(id, layer.source)
            .context("Context source missing")?;
        let pixels = frame_pixels(source)?;
        ensure!(
            source.format == 2
                && pixels.as_chunks::<4>().0.iter().any(|p| p[3] == 0)
                && pixels
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .all(|p| p[..3].iter().all(|v| *v <= p[3].saturating_add(1))),
            "Context alpha/premultiplication lost"
        );
        ensure!(
            mode != 1 || c.composed.missing_layers == missing_before,
            "Context missing-layer diagnostics"
        );
        ensure!(
            c.resources.resource_count() <= credits as usize,
            "Context export pool exceeded"
        );
        desktop.check(c.raw.pid, "M5 streamed right-click context menu")?;
        c.press(&w, 27, "Escape", "Escape")?;
        c.wait_hosts(id, false)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let restored = c.frame(id)?;
            if c.scenes.get(&id).is_some_and(|s| s.layers.len() == 1) {
                let pixels = frame_pixels(&restored)?;
                ensure!(
                    pixels[page_top * base.width as usize * 4..]
                        == base_pixels[page_top * base.width as usize * 4..],
                    "Context close left ghosts"
                );
                break;
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "Context close deadline"
            );
        }
        let before = context_title(c, id)?;
        context_click(c, &w, x, y)?;
        c.wait_hosts(id, true)?;
        let reload = c
            .menu_items
            .iter()
            .find(|(command, _)| *command == 33002)
            .map(|(_, rect)| rect.clone())
            .with_context(|| format!("Reload geometry missing: {:?}", c.menu_items))?;
        c.click(
            &w,
            reload.x + reload.width / 2.,
            reload.y + reload.height / 2.,
        )?;
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let title = context_title(c, id)?;
            if title.starts_with("context-") && title != before {
                break;
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "Context Reload click did not rerun page script"
            );
            c.pump_for(Duration::from_millis(10))?;
        }
        c.wait_hosts(id, false)?;
        ensure!(
            c.faults.len() == faults_before,
            "Context input faults: {:?}",
            &c.faults[faults_before..]
        );
        println!(
            "PASS decoded context-menu alpha/close/Reload selection {width}x{height}@{dpr} mode={mode} credits={credits} changed={changed}/{decoded_changed}"
        );
    }
    Ok(())
}
