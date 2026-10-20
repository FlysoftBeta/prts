impl Client {
    fn pump_for(&mut self, duration: Duration) -> Result<()> {
        let deadline = std::time::Instant::now() + duration;
        while std::time::Instant::now() < deadline {
            match self.rx.recv_timeout(Duration::from_millis(5)) {
                Ok(record) => self.consume(record?)?,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
}
fn frame_pixels(frame: &prtsd::browser_frames::CpuFrame) -> Result<Vec<u8>> {
    if let Some(gpu) = &frame.texture {
        gpu.gpu.lock().unwrap().readback(&gpu.texture)
    } else {
        Ok(frame.pixels.clone())
    }
}
fn decode_composite(
    encoder: &mut prtsd::encoder::Selected,
    frame: &prtsd::browser_frames::CpuFrame,
) -> Result<Vec<u8>> {
    use openh264::formats::YUVSource;
    use prtsd::encoder::VideoEncoder;
    let data = if let Some(gpu) = &frame.texture {
        encoder.encode_texture(gpu, true)?
    } else {
        encoder.encode(
            &frame.pixels,
            frame.width as usize,
            frame.height as usize,
            true,
        )?
    };
    let mut decoder = openh264::decoder::Decoder::new()?;
    let mut rgb = None;
    for nal in openh264::nal_units(&data) {
        if let Some(pic) = decoder.decode(nal)? {
            let (w, h) = pic.dimensions();
            let mut padded = vec![0; w * h * 3];
            pic.write_rgb8(&mut padded);
            let mut image = Vec::new();
            for row in padded.chunks_exact(w * 3).take(frame.height as usize) {
                image.extend_from_slice(&row[..frame.width as usize * 3]);
            }
            rgb = Some(image);
        }
    }
    rgb.context("Popup composite did not decode immediately")
}
fn flyout_gate(c: &mut Client, id: u64, desktop: &DesktopProbe) -> Result<()> {
    let faults_before = c.faults.len();
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
        .context("Missing flyout owner")?;
    let mut navigate = w.active_tab.to_le_bytes().to_vec();
    v1::string(
        "data:text/html,<body style='margin:0;background:rgb(220,30,60)'>",
        &mut navigate,
    )?;
    ensure!(
        c.request(TAB_NAVIGATE, navigate)?.kind == v1::OK,
        "Fixture navigation failed"
    );
    let mut config = id.to_le_bytes().to_vec();
    config.push(mode);
    config.extend(credits.to_le_bytes());
    config.extend(30u32.to_le_bytes());
    ensure!(
        c.request(WINDOW_FRAME_CONFIG, config)?.kind == WINDOW_RESULT,
        "Flyout capture config failed"
    );
    for (width, height, dpr) in [(960, 640, 1.), (960, 640, 2.), (961, 641, 1.25)] {
        let mut resize = id.to_le_bytes().to_vec();
        Size { width, height, dpr }.encode(&mut resize)?;
        let response = c.request(WINDOW_RESIZE, resize)?;
        ensure!(response.kind == WINDOW_RESULT, "Flyout resize failed");
        let w = Window::decode(&response.payload)?;
        c.geometry(id)?;
        c.send(&w, v1::Input::Focus(true), 0)?;
        let paint_deadline = std::time::Instant::now() + Duration::from_secs(5);
        let (base, base_pixels) = loop {
            let frame = c.frame(id)?;
            let pixels = frame_pixels(&frame)?;
            let at = ((frame.height as usize * 3 / 4) * frame.width as usize
                + frame.width as usize / 2)
                * 4;
            if pixels[at..at + 3] == [60, 30, 220] {
                break (frame, pixels);
            }
            ensure!(
                std::time::Instant::now() < paint_deadline,
                "Flyout base page did not paint"
            );
        };
        let preference = if mode == 1 { "qsv" } else { "software" };
        let (mut encoder, warning) =
            prtsd::encoder::Selected::new(preference, base.width, base.height, 4_000_000, 30)?;
        ensure!(warning.is_none(), "Popup encoder fallback: {warning:?}");
        c.wait_hosts(id, false)?;
        // Keep returning resource credits during Chrome's 100ms menu-button
        // debounce, just as the production media loop does.
        c.pump_for(Duration::from_millis(150))?;
        let button = c.menu_button.clone().context("No menu button geometry")?;
        let opened = std::time::Instant::now();
        c.click(
            &w,
            button.x + button.width / 2.,
            button.y + button.height / 2.,
        )?;
        let deadline = opened + Duration::from_millis(250);
        let (composite, layer) = loop {
            let frame = c.frame(id)?;
            let popup = c
                .scenes
                .get(&id)
                .and_then(|s| s.layers.iter().find(|l| l.role != 1))
                .cloned();
            if let Some(layer) = popup
                && c.composed.cached_frame(id, layer.source).is_some()
            {
                let image = frame_pixels(&frame)?;
                let left = (layer.rect.x * dpr).round() as usize;
                let top = ((layer.rect.y * dpr).round() as usize).max((160. * dpr) as usize);
                let right = (left + layer.width as usize).min(frame.width as usize);
                let bottom = (top + layer.height as usize).min(frame.height as usize);
                let mut visible = 0;
                for y in top..bottom {
                    for x in left..right {
                        let at = (y * frame.width as usize + x) * 4;
                        if image[at..at + 3]
                            .iter()
                            .zip(&base_pixels[at..at + 3])
                            .any(|(a, b)| a.abs_diff(*b) > 50)
                        {
                            visible += 1;
                        }
                    }
                }
                println!(
                    "POPUP candidate visible={visible} elapsed={}ms",
                    opened.elapsed().as_millis()
                );
                // Views can legitimately submit a fully transparent animation
                // frame. Appearance is the first visibly rendered popup, still
                // subject to the same end-to-end 250ms deadline below.
                if visible > 1000 {
                    break (frame, layer);
                }
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "Popup first-frame deadline exceeded"
            );
        };
        ensure!(
            opened.elapsed() <= Duration::from_millis(250),
            "Popup show latency {}ms",
            opened.elapsed().as_millis()
        );
        ensure!(
            (mode == 1) == composite.texture.is_some(),
            "Unexpected popup composition transport"
        );
        let source = c
            .composed
            .cached_frame(id, layer.source)
            .context("Popup source missing")?;
        let source_pixels = frame_pixels(source)?;
        let transparent = source_pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] == 0)
            .count();
        let partial = source_pixels
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] > 0 && p[3] < 255)
            .count();
        ensure!(
            source.format == 2 && transparent > 0 && partial > 0,
            "Popup alpha lost: format={} transparent={transparent} partial={partial}",
            source.format
        );
        ensure!(
            source_pixels
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| p[..3].iter().all(|v| *v <= p[3].saturating_add(1))),
            "Popup pixels are not premultiplied"
        );
        let pixels = frame_pixels(&composite)?;
        let rgb = decode_composite(&mut encoder, &composite)?;
        ensure!(
            opened.elapsed() <= Duration::from_millis(250),
            "Decoded popup show latency {}ms",
            opened.elapsed().as_millis()
        );
        let left = (layer.rect.x * dpr).round() as usize;
        let top = ((layer.rect.y * dpr).round() as usize).max((160. * dpr) as usize);
        let right = (left + layer.width as usize).min(base.width as usize);
        let bottom = (top + layer.height as usize).min(base.height as usize);
        let mut changed = 0;
        let mut decoded_changed = 0;
        for y in top..bottom {
            for x in left..right {
                let i = (y * base.width as usize + x) * 4;
                let j = (y * base.width as usize + x) * 3;
                if pixels[i..i + 3]
                    .iter()
                    .zip(&base_pixels[i..i + 3])
                    .any(|(a, b)| a.abs_diff(*b) > 50)
                {
                    changed += 1;
                }
                if rgb[j..j + 3]
                    .iter()
                    .zip([base_pixels[i + 2], base_pixels[i + 1], base_pixels[i]])
                    .any(|(a, b)| a.abs_diff(b) > 50)
                {
                    decoded_changed += 1;
                }
            }
        }
        println!(
            "POPUP PIXELS layer={layer:?} changed={changed}/{decoded_changed} transparent={transparent} partial={partial} elapsed={}ms",
            opened.elapsed().as_millis()
        );
        if changed <= 1000 {
            for (name, data, width, height) in [
                (
                    "source",
                    source_pixels.as_slice(),
                    layer.width,
                    layer.height,
                ),
                (
                    "composite",
                    pixels.as_slice(),
                    composite.width,
                    composite.height,
                ),
            ] {
                let mut ppm = format!("P6\n{width} {height}\n255\n").into_bytes();
                for p in data.as_chunks::<4>().0 {
                    ppm.extend([p[2], p[1], p[0]]);
                }
                std::fs::write(format!("C:/src/chromium/prts-flyout-{name}.ppm"), ppm)?;
            }
        }
        ensure!(
            changed > 1000 && decoded_changed > 1000,
            "Popup absent from composite/decoded pixels: {changed}/{decoded_changed}"
        );
        ensure!(
            c.composed.missing_layers == 0,
            "Missing popup-layer diagnostics"
        );
        ensure!(
            c.resources.resource_count() <= credits as usize,
            "Negotiated export pool exceeded"
        );
        desktop.check(c.raw.pid, "M5 visible streamed app-menu")?;
        c.press(&w, 27, "Escape", "Escape")?;
        c.wait_hosts(id, false)?;
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            let restored = c.frame(id)?;
            if c.scenes.get(&id).is_some_and(|s| s.layers.len() == 1) {
                let restored = frame_pixels(&restored)?;
                let page_top = (160. * dpr) as usize;
                // UI focus/hover may legitimately change; static page pixels
                // under the former menu must restore exactly, without ghosts.
                ensure!(
                    restored[page_top * base.width as usize * 4..]
                        == base_pixels[page_top * base.width as usize * 4..],
                    "Popup close did not restore static base"
                );
                break;
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "Popup scene did not close"
            );
        }
        println!(
            "PASS decoded app-menu alpha/composition/close {width}x{height}@{dpr} mode={mode} credits={credits}; transparent={transparent} partial={partial} changed={changed}/{decoded_changed}"
        );
        // Select a real Chrome command through the streamed menu host. The
        // test hook supplies geometry only; input uses the public Aura path.
        let before = c.list()?.into_iter().find(|w| w.id == id).unwrap();
        c.pump_for(Duration::from_millis(150))?;
        c.click(
            &w,
            button.x + button.width / 2.,
            button.y + button.height / 2.,
        )?;
        c.wait_hosts(id, true)?;
        let new_tab = c
            .menu_items
            .iter()
            .find(|(command, _)| *command == 34014)
            .map(|(_, rect)| rect.clone())
            .with_context(|| format!("New tab menu geometry missing: {:?}", c.menu_items))?;
        ensure!(new_tab.width > 0. && new_tab.height > 0., "Empty menu item");
        println!("MENU New tab rect={new_tab:?} hosts={:?}", c.hosts(id)?);
        c.click(
            &w,
            new_tab.x + new_tab.width / 2.,
            new_tab.y + new_tab.height / 2.,
        )?;
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        let created = loop {
            let current = c.list()?.into_iter().find(|w| w.id == id).unwrap();
            if current.tabs.len() == before.tabs.len() + 1 {
                break current.active_tab;
            }
            ensure!(
                std::time::Instant::now() < deadline,
                "Menu click did not create tab; hosts={:?} faults={:?}",
                c.hosts(id)?,
                c.faults
            );
            c.pump_for(Duration::from_millis(10))?;
        };
        ensure!(
            !before.tabs.iter().any(|tab| tab.id == created),
            "Menu command did not select the new tab"
        );
        c.wait_hosts(id, false)?;
        ensure!(
            c.request(TAB_CLOSE, created.to_le_bytes().to_vec())?.kind == v1::OK,
            "Close menu-created tab failed"
        );
        println!("PASS app-menu pointer selection creates real tab {width}x{height}@{dpr}");
        ensure!(
            c.faults.len() == faults_before,
            "Popup input generated faults: {:?}",
            &c.faults[faults_before..]
        );
    }
    Ok(())
}
