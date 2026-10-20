struct InputFixture {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    port: u16,
}
impl InputFixture {
    fn start(child_port: Option<u16>) -> Result<Self> {
        use std::io::Read;
        let listener = std::net::TcpListener::bind(if child_port.is_some() {
            "127.0.0.1:0"
        } else {
            "127.0.0.2:0"
        })?;
        listener.set_nonblocking(true)?;
        let port = listener.local_addr()?.port();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        let thread = thread::spawn(move || {
            let mut workers = Vec::new();
            while !flag.load(std::sync::atomic::Ordering::Relaxed) {
                if let Ok((mut stream, _)) = listener.accept() {
                    workers.push(thread::spawn(move || {
                    // Winsock accept inherits the listener's nonblocking mode.
                    // Wait for complete HTTP headers instead of dropping a
                    // connection on WSAEWOULDBLOCK before request bytes arrive.
                    if let Err(error) = stream.set_nonblocking(false) {
                        eprintln!("M5 fixture blocking mode failed: {error:?}");
                        return;
                    }
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    let mut request = [0; 8192];
                    let mut n=0;
                    while !request[..n].windows(4).any(|v|v==b"\r\n\r\n") {
                      if n==request.len(){return;}
                      match stream.read(&mut request[n..]){
                        Ok(0)=>return,
                        Err(error)=>{eprintln!("M5 fixture HTTP read failed after {n} bytes: {error:?}");return;},
                        Ok(count)=>n+=count
                      }
                    }
                    if std::env::var_os("PRTS_M5_FIXTURE_LOG").is_some() {
                        eprintln!("M5 fixture port={port} request={}", String::from_utf8_lossy(&request[..n]).lines().next().unwrap_or(""));
                    }
                    let blue = String::from_utf8_lossy(&request[..n]).contains("/blue");
                    let iframe=child_port.map_or(String::new(),|p|format!("<iframe onload='this.contentWindow.postMessage(1,\"*\")' style='position:absolute;left:320px;top:0;width:280px;height:280px;border:0' src='http://127.0.0.2:{p}/child'></iframe>"));
                    let report = if child_port.is_some() {
                        "document.title='M5:'+JSON.stringify(s)"
                    } else {
                        "parent.postMessage(s,'*')"
                    };
                    let color = if blue { "20,60,210" } else { "220,30,60" };
                    let body = format!(
                        r#"<!doctype html><meta charset=utf-8><style>body{{margin:0;height:2400px;background:rgb({color})}}button{{position:absolute;left:20px;top:20px;width:100px;height:30px;cursor:pointer}}input{{position:absolute;left:20px;top:80px;width:200px;height:30px}}textarea{{position:absolute;left:20px;top:180px;width:220px;height:40px}}#plain{{position:absolute;left:680px;top:180px;width:100px;height:60px;cursor:default}}</style><button id=b>click</button><form onsubmit='s.enters++;report();return false'><input id=t autocomplete=off></form><textarea id=sel>select this text across the window edge</textarea><div id=plain>plain</div>{iframe}<script>
let s={{url:location.href,clicks:0,mouse:[],value:'',events:[],keys:[],enters:0,scroll:0,child:null}};const held=new Set();b.onclick=()=>{{s.clicks++;report()}};
for(const e of ['mousedown','mouseup','click'])addEventListener(e,v=>{{s.mouse.push([e,v.clientX,v.clientY,v.target.id]);s.mouse=s.mouse.slice(-8);report()}});
for(const e of ['keydown','keypress','input','keyup'])t.addEventListener(e,()=>{{s.events.push(e);s.value=t.value;report()}});
addEventListener('keydown',e=>{{held.add(e.code);report()}});addEventListener('keyup',e=>{{held.delete(e.code);report()}});addEventListener('message',e=>{{s.child=e.data;report()}});
function report(){{s.keys=[...held];s.scroll=scrollY;s.dpr=devicePixelRatio;s.focus=document.hasFocus();s.active=document.activeElement.id;s.selection=sel.selectionEnd-sel.selectionStart;{report}}} addEventListener('scroll',report);sel.addEventListener('select',report);setInterval(report,25);report();
</script>"#
                    );
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    if let Err(error) = stream.write_all(response.as_bytes()) {
                        eprintln!("M5 fixture HTTP write failed: {error:?}");
                    }
                    let _ = stream.shutdown(std::net::Shutdown::Write);
                    }));
                } else {
                    thread::sleep(Duration::from_millis(2));
                }
            }
            for worker in workers {
                let _ = worker.join();
            }
        });
        Ok(Self {
            stop,
            thread: Some(thread),
            port,
        })
    }
}
impl Drop for InputFixture {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}
impl Client {
    fn send(&mut self, w: &Window, event: v1::Input, modifiers: u32) -> Result<()> {
        self.sequence += 1;
        let r = event.record(&v1::InputHeader {
            target: w.id,
            generation: w.generation,
            sequence: self.sequence,
            modifiers,
        })?;
        self.raw.command.write_all(&r.encode_version(2)?)?;
        Ok(())
    }
    fn click(&mut self, w: &Window, x: f64, y: f64) -> Result<()> {
        self.send(w, v1::Input::MouseMove { x, y, buttons: 0 }, 0)?;
        self.send(
            w,
            v1::Input::MouseDown {
                x,
                y,
                button: 1,
                buttons: 1,
                click_count: 1,
            },
            0,
        )?;
        self.send(
            w,
            v1::Input::MouseUp {
                x,
                y,
                button: 1,
                buttons: 0,
                click_count: 1,
            },
            0,
        )
    }
    fn key(
        &mut self,
        w: &Window,
        vk: u32,
        code: &str,
        key: &str,
        modifiers: u32,
        down: bool,
    ) -> Result<()> {
        let k = v1::Key {
            vk,
            code: code.into(),
            key: key.into(),
            text: String::new(),
            location: 0,
            repeat: false,
            is_system: false,
        };
        self.send(
            w,
            if down {
                v1::Input::KeyDown(k)
            } else {
                v1::Input::KeyUp(k)
            },
            modifiers,
        )
    }
    fn press(&mut self, w: &Window, vk: u32, code: &str, key: &str) -> Result<()> {
        self.key(w, vk, code, key, 0, true)?;
        self.key(w, vk, code, key, 0, false)
    }
    fn shortcut(&mut self, w: &Window, letter: char) -> Result<()> {
        self.key(w, 17, "ControlLeft", "Control", 2, true)?;
        let code = format!("Key{letter}");
        let key = letter.to_ascii_lowercase().to_string();
        self.key(w, letter as u32, &code, &key, 2, true)?;
        self.key(w, letter as u32, &code, &key, 2, false)?;
        self.key(w, 17, "ControlLeft", "Control", 0, false)
    }
    fn wait_window(&mut self, id: u64, condition: impl Fn(&Window) -> bool) -> Result<Window> {
        let end = std::time::Instant::now() + Duration::from_secs(8);
        loop {
            let windows = self.list()?;
            if let Some(w) = windows.into_iter().find(|w| w.id == id) {
                if condition(&w) {
                    return Ok(w);
                }
                ensure!(
                    std::time::Instant::now() < end,
                    "Window metadata condition timed out: {w:?}; faults={:?}",
                    self.faults
                );
            } else {
                ensure!(std::time::Instant::now() < end, "Window disappeared");
            }
            if let Ok(r) = self.rx.recv_timeout(Duration::from_millis(10)) {
                self.consume(r?)?;
            }
        }
    }
    fn state(
        &mut self,
        id: u64,
        condition: impl Fn(&serde_json::Value) -> bool,
    ) -> Result<serde_json::Value> {
        let mut value = None;
        self.wait_window(id, |w| {
            w.tabs
                .iter()
                .find(|t| t.id == w.active_tab)
                .and_then(|t| t.title.strip_prefix("M5:"))
                .and_then(|t| serde_json::from_str::<serde_json::Value>(t).ok())
                .is_some_and(|s| condition(&s))
        })?;
        // Fetch the exact observed state after the predicate; fixture fields
        // are monotonic except scroll, whose predicate tolerances are explicit.
        for w in self.list()? {
            if w.id == id {
                value = w
                    .tabs
                    .iter()
                    .find(|t| t.id == w.active_tab)
                    .and_then(|t| t.title.strip_prefix("M5:"))
                    .and_then(|t| serde_json::from_str(t).ok());
            }
        }
        value.context("No fixture state")
    }
    fn navigate_tab(&mut self, tab: u64, url: &str) -> Result<()> {
        let mut p = tab.to_le_bytes().to_vec();
        v1::string(url, &mut p)?;
        ensure!(
            self.request(TAB_NAVIGATE, p)?.kind == v1::OK,
            "Navigation rejected"
        );
        Ok(())
    }
    fn resize_window(&mut self, id: u64, dsf: f64) -> Result<Window> {
        let mut p = id.to_le_bytes().to_vec();
        Size {
            width: 960,
            height: 640,
            dpr: dsf,
        }
        .encode(&mut p)?;
        let r = self.request(WINDOW_RESIZE, p)?;
        ensure!(r.kind == WINDOW_RESULT, "Resize failed {r:?}");
        Window::decode(&r.payload)
    }
    fn wait_cursor(&mut self, w: &Window, x: f64, y: f64, shape: u8) -> Result<()> {
        let start = self.cursors.len();
        self.send(w, v1::Input::MouseMove { x, y, buttons: 0 }, 0)?;
        let end = std::time::Instant::now() + Duration::from_secs(5);
        while !self.cursors[start..]
            .iter()
            .any(|(id, g, s)| *id == w.id && *g == w.generation && *s == shape)
        {
            ensure!(
                std::time::Instant::now() < end,
                "Missing window cursor{shape} at{x},{y}; events={:?}",
                &self.cursors[start..]
            );
            self.list()?;
        }
        Ok(())
    }
    fn decoded_page(&mut self, id: u64, color: [u8; 3]) -> Result<()> {
        use prtsd::encoder::VideoEncoder;
        let end = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let f = self.frame(id)?;
            let pixels = if let Some(t) = &f.texture {
                t.gpu.lock().unwrap().readback(&t.texture)?
            } else {
                f.pixels.clone()
            };
            let i = ((f.height as usize * 3 / 4) * f.width as usize + f.width as usize * 3 / 4) * 4;
            if !pixels[i..i + 3]
                .iter()
                .zip([color[2], color[1], color[0]])
                .all(|(a, b)| a.abs_diff(b) < 5)
            {
                ensure!(
                    std::time::Instant::now() < end,
                    "Decoded tab page never changed"
                );
                continue;
            }
            let mut encoder = prtsd::encoder::Software::new(4_000_000, 30)?;
            let mut decoder = openh264::decoder::Decoder::new()?;
            let bytes = encoder.encode(&pixels, f.width as usize, f.height as usize, true)?;
            let mut decoded = false;
            for nal in openh264::nal_units(&bytes) {
                if let Some(pic) = decoder.decode(nal)? {
                    let mut rgb = vec![0; f.width as usize * f.height as usize * 3];
                    pic.write_rgb8(&mut rgb);
                    let i = ((f.height as usize * 3 / 4) * f.width as usize
                        + f.width as usize * 3 / 4)
                        * 3;
                    ensure!(
                        rgb[i..i + 3]
                            .iter()
                            .zip(color)
                            .all(|(a, b)| a.abs_diff(b) < 25),
                        "Tab switch decoded wrong page"
                    );
                    decoded = true;
                }
            }
            ensure!(decoded, "Tab video did not decode");
            return Ok(());
        }
    }
}
fn input_gate(c: &mut Client, id: u64) -> Result<()> {
    let child = InputFixture::start(None)?;
    let fixture = InputFixture::start(Some(child.port))?;
    let red = format!("http://127.0.0.1:{}/red", fixture.port);
    let blue = format!("http://127.0.0.1:{}/blue", fixture.port);
    let mut w = c.resize_window(id, 1.)?;
    c.navigate_tab(w.active_tab, &red)?;
    c.state(id, |s| s["child"].is_object())?;
    let original = w.active_tab;
    let second = c.tab(id, &blue)?;
    c.state(id, |s| s["child"].is_object())?;
    let mut config = id.to_le_bytes().to_vec();
    config.push(
        if std::env::var("PRTS_FRAME_MODE").as_deref() == Ok("cpu") {
            2
        } else {
            1
        },
    );
    config.extend(2u32.to_le_bytes());
    config.extend(30u32.to_le_bytes());
    ensure!(
        c.request(WINDOW_FRAME_CONFIG, config)?.kind == WINDOW_RESULT,
        "Input capture subscription rejected"
    );
    c.faults.clear();
    c.send(&w, v1::Input::Focus(true), 0)?;
    for (index, tab, color) in [(0, original, [220, 30, 60]), (1, second.id, [20, 60, 210])] {
        let (_, _, _, rects) = c.geometry(id)?;
        let tab_rect = &rects[4 + index];
        c.click(
            &w,
            tab_rect.x + tab_rect.width / 2.,
            tab_rect.y + tab_rect.height / 2.,
        )?;
        w = c.wait_window(id, |v| v.active_tab == tab)?;
        c.decoded_page(id, color)?;
    }
    println!("PASS real Aura tab-strip clicks switch metadata and decoded page colors");
    let (_, _, _, rects) = c.geometry(id)?;
    let omni = &rects[2];
    c.click(&w, omni.x + omni.width / 2., omni.y + omni.height / 2.)?;
    c.shortcut(&w, 'A')?;
    c.send(&w, v1::Input::Char(red.clone()), 0)?;
    c.press(&w, 13, "Enter", "Enter")?;
    w = c.wait_window(id, |v| {
        v.tabs
            .iter()
            .any(|t| t.active && t.url == red && t.load == 2)
    })?;
    c.state(id, |s| s["child"].is_object())?;
    println!("PASS omnibox Enter navigated red and child ready");
    c.shortcut(&w, 'L')?;
    c.send(&w, v1::Input::Char(blue.clone()), 0)?;
    c.press(&w, 13, "Enter", "Enter")?;
    w = c.wait_window(id, |v| {
        v.tabs
            .iter()
            .any(|t| t.active && t.url == blue && t.load == 2)
    })?;
    c.state(id, |s| s["child"].is_object())?;
    println!("PASS Ctrl+L Enter navigated blue and child ready; sending Ctrl+T");
    c.shortcut(&w, 'T')?;
    w = c.wait_window(id, |v| {
        v.tabs.len() == 3 && v.tabs.iter().any(|t| t.active && t.load == 2)
    })?;
    println!("PASS Ctrl+T created tab; sending Ctrl+W");
    c.shortcut(&w, 'W')?;
    w = c.wait_window(id, |v| v.tabs.len() == 2)?;
    println!("PASS omnibox click/typing/Enter navigation and Ctrl+L/T/W");
    for dsf in [1., 2.] {
        w = c.resize_window(id, dsf)?;
        let destination = format!("{red}?dsf={dsf}");
        c.navigate_tab(w.active_tab, &destination)?;
        c.state(id, |s| {
            s["url"] == destination && s["dpr"].as_f64() == Some(dsf) && s["child"].is_object()
        })?;
        let (_, _, _, rects) = c.geometry(id)?;
        let page = &rects[3];
        let omni = &rects[2];
        ensure!(
            c.widget_count >= 2,
            "Cross-origin fixture has no separate renderer widget"
        );
        let (px, py) = (page.x, page.y);
        for (offset, child_input) in [(0., false), (320., true)] {
            c.click(&w, px + offset + 60., py + 35.)?;
            c.click(&w, px + offset + 60., py + 95.)?;
            // No sleeps or metadata queries between click and all following
            // key/commit actions: this exercises Aura/OOPIF focus completion.
            c.key(&w, 65, "KeyA", "a", 0, true)?;
            c.send(&w, v1::Input::Char("a".into()), 0)?;
            c.key(&w, 65, "KeyA", "a", 0, false)?;
            c.send(&w, v1::Input::Char("\u{1f600}汉字".into()), 0)?;
            c.press(&w, 8, "Backspace", "Backspace")?;
            c.press(&w, 13, "Enter", "Enter")?;
            let state = c.state(id, |s| {
                let s = if child_input { &s["child"] } else { s };
                s["value"] == "a\u{1f600}汉" && s["enters"] == 1 && s["clicks"] == 1
            })?;
            let s = if child_input { &state["child"] } else { &state };
            ensure!(
                s["events"]
                    .as_array()
                    .context("No key events")?
                    .iter()
                    .filter(|e| **e == "input")
                    .count()
                    == 3,
                "Duplicate/missing text input {s}"
            );
        }
        c.wait_cursor(&w, px + 60., py + 35., 1)?;
        c.wait_cursor(&w, omni.x + omni.width / 2., omni.y + omni.height / 2., 2)?;
        c.wait_cursor(&w, px + 720., py + 200., 0)?;
        c.wait_cursor(&w, px + 60., py + 35., 1)?;
        c.wait_cursor(&w, px + 60., py + 95., 2)?;
        c.send(
            &w,
            v1::Input::Wheel {
                x: px + 720.,
                y: py + 250.,
                delta_x: 0.,
                delta_y: 120.,
                buttons: 0,
            },
            0,
        )?;
        let s = c.state(id, |s| {
            s["scroll"]
                .as_f64()
                .is_some_and(|v| (110. ..=130.).contains(&v))
        })?;
        ensure!(
            s["scroll"].as_f64().unwrap() <= 130.,
            "Wheel DSF multiplied delta"
        );
        c.send(
            &w,
            v1::Input::Wheel {
                x: px + 720.,
                y: py + 250.,
                delta_x: 0.,
                delta_y: -60.,
                buttons: 0,
            },
            0,
        )?;
        c.state(id, |s| {
            s["scroll"]
                .as_f64()
                .is_some_and(|v| (50. ..=70.).contains(&v))
        })?;
        c.send(
            &w,
            v1::Input::Wheel {
                x: px + 720.,
                y: py + 250.,
                delta_x: 0.,
                delta_y: -60.,
                buttons: 0,
            },
            0,
        )?;
        c.state(id, |s| s["scroll"] == 0)?;
        for _ in 0..4 {
            c.send(
                &w,
                v1::Input::Wheel {
                    x: px + 720.,
                    y: py + 250.,
                    delta_x: 0.,
                    delta_y: 0.25,
                    buttons: 0,
                },
                0,
            )?;
        }
        c.state(id, |s| s["scroll"] == 1)?;
        c.send(
            &w,
            v1::Input::MouseDown {
                x: px + 25.,
                y: py + 200.,
                button: 1,
                buttons: 1,
                click_count: 1,
            },
            0,
        )?;
        c.send(
            &w,
            v1::Input::MouseMove {
                x: 1100.,
                y: py + 215.,
                buttons: 1,
            },
            0,
        )?;
        c.send(
            &w,
            v1::Input::MouseUp {
                x: 1100.,
                y: py + 215.,
                button: 1,
                buttons: 0,
                click_count: 1,
            },
            0,
        )?;
        c.state(id, |s| s["selection"].as_i64().is_some_and(|v| v > 0))?;
        c.key(&w, 16, "ShiftLeft", "Shift", 1, true)?;
        c.send(
            &w,
            v1::Input::MouseDown {
                x: px + 60.,
                y: py + 95.,
                button: 1,
                buttons: 1,
                click_count: 1,
            },
            1,
        )?;
        c.send(&w, v1::Input::Focus(false), 0)?;
        c.send(&w, v1::Input::Focus(true), 0)?;
        c.click(&w, px + 60., py + 35.)?;
        c.state(id, |s| {
            s["clicks"] == 2 && s["keys"].as_array().is_some_and(|k| k.is_empty())
        })?;
        c.geometry(id)?;
        ensure!(
            c.faults.is_empty(),
            "Unexpected input faults DSF{dsf}: {:?}",
            c.faults
        );
        println!(
            "PASS DSF{dsf}: no-sleep main/OOPIF click/text exact single insertion, Unicode, Backspace/Enter, wheel sign/precision, UI/page cursor, captured outside selection, blur release"
        );
    }
    let (_, _, _, rects) = c.geometry(id)?;
    let page = &rects[3];
    c.send(
        &w,
        v1::Input::MouseDown {
            x: page.x + 60.,
            y: page.y + 95.,
            button: 1,
            buttons: 1,
            click_count: 1,
        },
        0,
    )?;
    c.send(&w, v1::Input::Char("retired-navigation".into()), 0)?;
    let destination = format!("{blue}?race=navigation");
    c.navigate_tab(w.active_tab, &destination)?;
    c.state(id, |s| {
        s["url"] == destination && s["child"].is_object() && s["value"] == ""
    })?;
    c.click(&w, page.x + 60., page.y + 35.)?;
    c.state(id, |s| s["clicks"] == 1)?;
    ensure!(
        c.faults.is_empty(),
        "Navigation race leaked completion fault {:?}",
        c.faults
    );
    println!(
        "PASS navigation cancels old queued input; new page receives no ghost text/held pointer"
    );
    let other = c.create(&red)?;
    c.send(&other, v1::Input::Focus(true), 0)?;
    c.send(&w, v1::Input::Char("cross-owner".into()), 0)?;
    c.request(v1::PING, 123u64.to_le_bytes().to_vec())?;
    ensure!(
        c.faults.pop() == Some(10),
        "Cross-owner window input accepted"
    );
    c.state(other.id, |s| s["url"] == red && s["child"].is_object())?;
    let (_, _, _, rects) = c.geometry(other.id)?;
    let page = &rects[3];
    c.send(
        &other,
        v1::Input::MouseDown {
            x: page.x + 60.,
            y: page.y + 95.,
            button: 1,
            buttons: 1,
            click_count: 1,
        },
        0,
    )?;
    c.send(&other, v1::Input::Char("closing-input".into()), 0)?;
    ensure!(
        c.request(WINDOW_CLOSE, other.id.to_le_bytes().to_vec())?
            .kind
            == v1::OK,
        "Close rejected"
    );
    c.send(&other, v1::Input::Char("stale".into()), 0)?;
    c.request(v1::PING, 124u64.to_le_bytes().to_vec())?;
    ensure!(c.faults.pop() == Some(10), "Closed window input accepted");
    c.send(&w, v1::Input::Focus(true), 0)?;
    c.geometry(id)?;
    println!(
        "PASS window owner switch, stale/closed window input and native OS cursor write count0"
    );
    println!("PASS close during pending routed input drains/cancels without ghost delivery");
    println!("PASS M5 step3 Aura input gate");
    Ok(())
}
