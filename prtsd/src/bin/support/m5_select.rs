fn select_gate(c: &mut Client, id: u64, desktop: &DesktopProbe) -> Result<()> {
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
        "Select capture config failed"
    );
    for (width, height, dpr) in [(960, 640, 1.), (960, 640, 2.), (961, 641, 1.25)] {
        let mut resize = id.to_le_bytes().to_vec();
        Size { width, height, dpr }.encode(&mut resize)?;
        let response = c.request(WINDOW_RESIZE, resize)?;
        ensure!(response.kind == WINDOW_RESULT, "Select resize failed");
        let w = Window::decode(&response.payload)?;
        let (_, _, _, rects) = c.geometry(id)?;
        let mut navigate = w.active_tab.to_le_bytes().to_vec();
        v1::string(
            "data:text/html,<body style='margin:0;background:rgb(220,30,60)'><select id=s style='position:absolute;left:30px;top:100px;width:260px;height:40px;font-size:20px'><option value=red>Red default</option><option value=green>Green middle</option><option value=blue>Blue selected</option></select><script>document.title='choice:red';s.onchange=()=>{document.title='choice:'+s.value;document.body.style.background=s.value==='blue'?'rgb(20,60,210)':'rgb(220,30,60)'}</script>",
            &mut navigate,
        )?;
        ensure!(
            c.request(TAB_NAVIGATE, navigate)?.kind == v1::OK,
            "Select fixture navigate failed"
        );
        c.send(&w, v1::Input::Focus(true), 0)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let (base, base_pixels) = loop {
            let frame = c.frame(id)?;
            let pixels = frame_pixels(&frame)?;
            let at = ((frame.height as usize * 3 / 4) * frame.width as usize
                + frame.width as usize / 2)
                * 4;
            if context_title(c, id)? == "choice:red" && pixels[at..at + 3] == [60, 30, 220] {
                break (frame, pixels);
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "Select fixture load/paint deadline"
            );
        };
        let preference = if mode == 1 { "qsv" } else { "software" };
        let (mut encoder, warning) =
            prtsd::encoder::Selected::new(preference, base.width, base.height, 4_000_000, 30)?;
        ensure!(warning.is_none(), "Select encoder fallback: {warning:?}");
        let x = rects[3].x + 160.;
        let y = rects[3].y + 120.;
        // Test only the page below the closed control, so its focus border
        // cannot masquerade as a visible dropdown or break close restoration.
        let popup_top = ((rects[3].y + 145.) * dpr).ceil() as usize;
        let opened = std::time::Instant::now();
        c.click(&w, x, y)?;
        let (composite, changed) = loop {
            let frame = c.frame(id)?;
            let pixels = frame_pixels(&frame)?;
            let changed = pixels[popup_top * frame.width as usize * 4..]
                .as_chunks::<4>()
                .0
                .iter()
                .zip(
                    base_pixels[popup_top * frame.width as usize * 4..]
                        .as_chunks::<4>()
                        .0
                        .iter(),
                )
                .filter(|(a, b)| a[..3].iter().zip(&b[..3]).any(|(a, b)| a.abs_diff(*b) > 50))
                .count();
            if changed > 1000 {
                break (frame, changed);
            }
            ensure!(
                opened.elapsed() <= Duration::from_millis(250),
                "Select dropdown appearance deadline; hosts={:?}",
                c.hosts(id)?
            );
        };
        ensure!(
            (mode == 1) == composite.texture.is_some(),
            "Select composition transport changed"
        );
        let rgb = decode_composite(&mut encoder, &composite)?;
        ensure!(
            opened.elapsed() <= Duration::from_millis(250),
            "Decoded select show latency {}ms",
            opened.elapsed().as_millis()
        );
        let decoded_changed = rgb[popup_top * composite.width as usize * 3..]
            .as_chunks::<3>()
            .0
            .iter()
            .zip(
                base_pixels[popup_top * composite.width as usize * 4..]
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
            "Select dropdown absent in decoded video"
        );
        ensure!(
            c.composed.missing_layers == missing_before,
            "Select missing-layer diagnostics"
        );
        ensure!(
            c.resources.resource_count() <= credits as usize,
            "Select pool exceeded"
        );
        desktop.check(c.raw.pid, "M5 streamed select dropdown")?;
        c.press(&w, 27, "Escape", "Escape")?;
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let frame = c.frame(id)?;
            let pixels = frame_pixels(&frame)?;
            if pixels[popup_top * frame.width as usize * 4..]
                == base_pixels[popup_top * frame.width as usize * 4..]
            {
                break;
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "Select close left dropdown ghosts"
            );
        }
        ensure!(
            context_title(c, id)? == "choice:red",
            "Escape committed select option"
        );
        c.click(&w, x, y)?;
        c.press(&w, 35, "End", "End")?;
        c.press(&w, 13, "Enter", "Enter")?;
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            let frame = c.frame(id)?;
            if context_title(c, id)? == "choice:blue" {
                let rgb = decode_composite(&mut encoder, &frame)?;
                let at = ((frame.height as usize * 3 / 4) * frame.width as usize
                    + frame.width as usize / 2)
                    * 3;
                if rgb[at..at + 3]
                    .iter()
                    .zip([20u8, 60, 210])
                    .all(|(a, b)| a.abs_diff(b) <= 15)
                {
                    break;
                }
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "Select End/Enter did not commit blue option in decoded video; title={:?}",
                context_title(c, id)?
            );
        }
        c.wait_hosts(id, false)?;
        ensure!(
            c.faults.len() == faults_before,
            "Select input faults: {:?}",
            &c.faults[faults_before..]
        );
        println!(
            "PASS decoded select popup/close/End-Enter choice {width}x{height}@{dpr} mode={mode} credits={credits} changed={changed}/{decoded_changed}"
        );
    }
    Ok(())
}
