//! Real input/cursor acceptance. No debugging endpoint or script injection API.
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    acceptance::run()
}
#[cfg(not(windows))]
fn main() {
    eprintln!("browser_m2 requires Windows");
}
#[cfg(windows)]
mod acceptance {
    use anyhow::{Result, bail, ensure};
    use prtsd::{
        browser::{Browser, DEFAULT_CHROME, RawBrowser},
        browser_wire::*,
    };
    use std::{
        io::{Read, Write},
        net::TcpListener,
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };
    use windows::Win32::{
        Foundation::{HWND, LPARAM},
        Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
        UI::WindowsAndMessaging::*,
    };
    use windows::core::BOOL;
    struct Server {
        stop: Arc<AtomicBool>,
        handle: Option<thread::JoinHandle<()>>,
        port: u16,
    }
    impl Server {
        fn start(child: bool, child_port: u16) -> Result<Self> {
            let listener = TcpListener::bind("127.0.0.1:0")?;
            let port = listener.local_addr()?.port();
            listener.set_nonblocking(true)?;
            let stop = Arc::new(AtomicBool::new(false));
            let flag = stop.clone();
            let handle = thread::spawn(move || {
                while !flag.load(Ordering::Relaxed) {
                    if let Ok((mut stream, _)) = listener.accept() {
                        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
                        let mut request = [0; 8192];
                        let _ = stream.read(&mut request);
                        let body = page(child, child_port);
                        let response = format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                            body.len(),
                            body
                        );
                        let _ = stream.write_all(response.as_bytes());
                    } else {
                        thread::sleep(Duration::from_millis(5));
                    }
                }
            });
            Ok(Self {
                stop,
                handle: Some(handle),
                port,
            })
        }
    }
    impl Drop for Server {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
            if let Some(t) = self.handle.take() {
                let _ = t.join();
            }
        }
    }
    fn page(child: bool, port: u16) -> String {
        let frame = if child {
            String::new()
        } else {
            format!(
                r#"<iframe style="position:absolute;left:300px;top:0;width:250px;height:250px;border:0" src="http://localhost:{port}/child"></iframe>"#
            )
        };
        format!(
            r#"<!doctype html><meta charset=utf-8><style>body{{margin:0;height:2200px}}button{{position:absolute;left:20px;top:20px;width:100px;height:30px;cursor:pointer}}input{{position:absolute;left:20px;top:80px;width:200px;height:30px}}#plain{{position:absolute;left:600px;top:180px;width:100px;height:60px;cursor:default}}</style><button id=b>click</button><input id=t><div id=plain>plain</div>{frame}<script>
 let s={{mouse:[],clicks:0,events:[],value:'',focus:false,scroll:0,dpr:devicePixelRatio,width:innerWidth,child:null}};
 b.onclick=()=>s.clicks++;
 for(let e of ['mousemove','mousedown','mouseup','click']) addEventListener(e,v=>{{s.mouse.push([e,v.clientX,v.clientY,v.target.tagName]);s.mouse=s.mouse.slice(-12)}});
 for(let e of ['keydown','keypress','input','keyup']) t.addEventListener(e,()=>{{s.events.push(e);s.value=t.value}});
 addEventListener('message',e=>{{s.child=e.data}});
 setInterval(()=>{{s.active=document.activeElement.id;s.focus=document.hasFocus();s.scroll=scrollY;s.dpr=devicePixelRatio;s.width=innerWidth;{report}}},40);
 </script>"#,
            report = if child {
                "parent.postMessage(s,'*')"
            } else {
                "document.title='M2:'+JSON.stringify(s)"
            }
        )
    }
    struct Driver {
        b: Browser,
        seq: u64,
        target: Target,
    }
    impl Driver {
        fn send(&mut self, event: Input) -> Result<()> {
            self.seq += 1;
            self.b.input(
                &InputHeader {
                    target: self.target.id,
                    generation: self.target.generation,
                    sequence: self.seq,
                    modifiers: 0,
                },
                &event,
            )
        }
        fn click(&mut self, x: f64, y: f64) -> Result<()> {
            self.send(Input::MouseMove { x, y, buttons: 0 })?;
            self.send(Input::MouseDown {
                x,
                y,
                button: 1,
                buttons: 1,
                click_count: 1,
            })?;
            self.send(Input::MouseUp {
                x,
                y,
                button: 1,
                buttons: 0,
                click_count: 1,
            })
        }
        fn state(
            &self,
            predicate: impl Fn(&serde_json::Value) -> bool,
        ) -> Result<serde_json::Value> {
            let deadline = Instant::now() + Duration::from_secs(10);
            let current = self.b.list()?;
            let mut last = current
                .iter()
                .find(|t| t.id == self.target.id)
                .and_then(|t| t.title.strip_prefix("M2:"))
                .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok());
            if let Some(value) = &last
                && predicate(value)
            {
                return Ok(value.clone());
            }

            while Instant::now() < deadline {
                if let Some(Event::Changed(t)) = self.b.receive_event(Duration::from_millis(100))?
                    && t.id == self.target.id
                    && t.title.starts_with("M2:")
                {
                    let value: serde_json::Value = serde_json::from_str(&t.title[3..])?;
                    if predicate(&value) {
                        return Ok(value);
                    }
                    last = Some(value);
                }
            }
            bail!(
                "Page state deadline; last={last:?}; snapshot={:?}",
                self.b.list()?
            )
        }
        fn cursor(&mut self, x: f64, y: f64, shape: u8) -> Result<()> {
            self.b.drain_events();
            self.send(Input::MouseMove { x, y, buttons: 0 })?;
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if let Some(Event::Cursor {
                    target,
                    generation,
                    cursor,
                }) = self.b.receive_event(Duration::from_millis(100))?
                    && target == self.target.id
                    && generation == self.target.generation
                    && cursor.shape == shape
                {
                    println!("cursor {shape} at {x},{y} passed");
                    return Ok(());
                }
            }
            bail!("Missing cursor shape {shape} at {x},{y}")
        }
        fn type_a(&mut self) -> Result<()> {
            let key = Key {
                vk: 65,
                code: "KeyA".into(),
                key: "a".into(),
                text: "a".into(),
                location: 0,
                repeat: false,
                is_system: false,
            };
            self.send(Input::KeyDown(key.clone()))?;
            self.send(Input::Char("a".into()))?;
            self.send(Input::KeyUp(Key {
                text: String::new(),
                ..key
            }))
        }
        fn fault(&self, code: u32) -> Result<()> {
            let deadline = Instant::now() + Duration::from_secs(5);
            while Instant::now() < deadline {
                if let Some(Event::Fault { code: c, fatal, .. }) =
                    self.b.receive_event(Duration::from_millis(100))?
                {
                    ensure!(c == code && !fatal, "Unexpected fault");
                    return Ok(());
                }
            }
            bail!("Missing fault {code}")
        }
    }
    struct Desktop {
        pid: u32,
        visible: bool,
    }
    unsafe extern "system" fn inspect(hwnd: HWND, l: LPARAM) -> BOOL {
        let state = unsafe { &mut *(l.0 as *mut Desktop) };
        let mut pid = 0;
        unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
        if pid == state.pid && unsafe { IsWindowVisible(hwnd) }.as_bool() {
            let mut cloak = 0u32;
            let _ = unsafe {
                DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, &mut cloak as *mut _ as *mut _, 4)
            };
            if cloak == 0 {
                state.visible = true
            }
        }
        BOOL(1)
    }
    fn desktop(pid: u32) -> Result<()> {
        let mut s = Desktop {
            pid,
            visible: false,
        };
        unsafe { EnumWindows(Some(inspect), LPARAM(&mut s as *mut _ as isize))? };
        ensure!(!s.visible, "Visible browser window");
        Ok(())
    }

    fn malformed_inputs() -> Result<()> {
        let path = std::env::var("PRTS_CHROME_PATH").unwrap_or_else(|_| DEFAULT_CHROME.into());
        let mut raw = RawBrowser::launch(std::path::Path::new(&path))?;
        raw.wait_startup()?;
        let mut reader = raw.event.try_clone()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            while let Ok(r) = Record::read(&mut reader) {
                if tx.send(r).is_err() {
                    break;
                }
            }
        });
        let mut hello = Vec::new();
        for v in [1u16, 0, 1, 0] {
            hello.extend(v.to_le_bytes());
        }
        hello.extend(std::process::id().to_le_bytes());
        hello.extend(0u64.to_le_bytes());
        hello.extend(2u32.to_le_bytes());
        hello.extend(3u32.to_le_bytes());
        Record {
            kind: HELLO,
            id: 1,
            payload: hello,
        }
        .write(&mut raw.command)?;
        ensure!(
            rx.recv_timeout(Duration::from_secs(5))?.kind == HELLO_ACK,
            "Missing raw ACK"
        );
        let mut p = Vec::new();
        string("about:blank", &mut p)?;
        Size {
            width: 800,
            height: 600,
            dpr: 1.,
        }
        .encode(&mut p)?;
        p.push(1);
        Record {
            kind: CREATE,
            id: 3,
            payload: p,
        }
        .write(&mut raw.command)?;
        let target = loop {
            let r = rx.recv_timeout(Duration::from_secs(5))?;
            if r.kind == TARGET_RESULT {
                break Decoder::new(&r.payload).target()?;
            }
        };
        let header = InputHeader {
            target: target.id,
            generation: target.generation,
            sequence: 1,
            modifiers: 0,
        };
        let movement = Input::MouseMove {
            x: 10.,
            y: 10.,
            buttons: 0,
        }
        .record(&header)?;
        let key = Key {
            vk: 65,
            code: "KeyA".into(),
            key: "a".into(),
            text: "a".into(),
            location: 0,
            repeat: false,
            is_system: false,
        };
        let mut cases: Vec<(&str, Record)> = Vec::new();
        let mut r = movement.clone();
        r.payload[28..36].copy_from_slice(&f64::NAN.to_le_bytes());
        cases.push(("NaN", r));
        let mut r = movement.clone();
        r.payload.pop();
        cases.push(("truncated", r));
        let mut r = movement.clone();
        r.payload.push(0);
        cases.push(("trailing", r));
        let mut r = movement.clone();
        r.payload[24..28].copy_from_slice(&64u32.to_le_bytes());
        cases.push(("modifier bits", r));
        let mut r = movement;
        r.payload[44..48].copy_from_slice(&32u32.to_le_bytes());
        cases.push(("button mask", r));
        let down = Input::MouseDown {
            x: 10.,
            y: 10.,
            button: 1,
            buttons: 1,
            click_count: 1,
        }
        .record(&header)?;
        let mut r = down.clone();
        r.payload[44] = 6;
        cases.push(("button enum", r));
        let mut r = down.clone();
        *r.payload.last_mut().unwrap() = 0;
        cases.push(("click count", r));
        let mut r = down;
        r.payload[45] = 0;
        cases.push(("down state", r));
        let down = Input::KeyDown(key).record(&header)?;
        let mut r = down.clone();
        r.kind = 0x205;
        cases.push(("key up text", r));
        let mut r = down.clone();
        let n = r.payload.len();
        r.payload[n - 3] = 4;
        cases.push(("key location", r));
        let mut r = down.clone();
        let n = r.payload.len();
        r.payload[n - 2] = 2;
        cases.push(("repeat bool", r));
        let mut r = down;
        *r.payload.last_mut().unwrap() = 2;
        cases.push(("system bool", r));
        let text = Input::Char("a".into()).record(&header)?;
        let mut r = text.clone();
        r.payload[32] = 0xff;
        cases.push(("UTF-8", r));
        let mut r = text.clone();
        r.payload[32] = 0;
        cases.push(("NUL", r));
        let mut r = text;
        r.payload.truncate(28);
        string(&"a".repeat(4097), &mut r.payload)?;
        cases.push(("text limit", r));
        let mut r = Input::Focus(true).record(&header)?;
        *r.payload.last_mut().unwrap() = 2;
        cases.push(("focus bool", r));
        for (i, (name, mut r)) in cases.into_iter().enumerate() {
            r.payload[16..24].copy_from_slice(&(i as u64 + 1).to_le_bytes());
            r.write(&mut raw.command)?;
            loop {
                let response = rx.recv_timeout(Duration::from_secs(5))?;
                if response.kind == FAULT {
                    ensure!(
                        matches!(
                            response.event()?,
                            Event::Fault {
                                code: 1,
                                fatal: false,
                                ..
                            }
                        ),
                        "Wrong fault for {name}"
                    );
                    break;
                }
            }
            println!("PASS browser validation: {name}");
        }
        // A burst of stale notifications must emit at most one diagnostic.
        for sequence in 100..120 {
            Input::Focus(true)
                .record(&InputHeader {
                    sequence,
                    generation: target.generation + 1,
                    ..header.clone()
                })?
                .write(&mut raw.command)?;
        }
        Record {
            kind: PING,
            id: 5,
            payload: 42u64.to_le_bytes().to_vec(),
        }
        .write(&mut raw.command)?;
        let mut faults = 0;
        loop {
            let r = rx.recv_timeout(Duration::from_secs(5))?;
            if r.kind == PONG {
                break;
            }
            if r.kind == FAULT {
                ensure!(
                    matches!(
                        r.event()?,
                        Event::Fault {
                            code: 10,
                            fatal: false,
                            ..
                        }
                    ),
                    "Wrong stale fault"
                );
                faults += 1;
            }
        }
        ensure!(faults == 1, "Stale burst emitted {faults} diagnostics");
        println!("PASS stale-state burst diagnostic rate limit");
        desktop(raw.pid)?;
        Record {
            kind: SHUTDOWN,
            id: 7,
            payload: vec![],
        }
        .write(&mut raw.command)?;
        loop {
            if rx.recv_timeout(Duration::from_secs(5))?.kind == OK {
                break;
            }
        }
        ensure!(
            raw.wait(Duration::from_secs(5)),
            "Raw validation browser did not exit"
        );
        worker
            .join()
            .map_err(|_| anyhow::anyhow!("raw reader panicked"))?;
        Ok(())
    }
    pub fn run() -> Result<()> {
        let foreground = unsafe { GetForegroundWindow() };
        if foreground.0.is_null() {
            ensure!(
                std::env::var_os("PRTS_M1_ALLOW_DISCONNECTED_DESKTOP").is_some(),
                "Disconnected desktop; explicit M1 override required"
            );
            println!("Foreground preservation UNVERIFIED: disconnected session");
        }
        let child = Server::start(true, 0)?;
        let main = Server::start(false, child.port)?;
        let path = std::env::var("PRTS_CHROME_PATH").unwrap_or_else(|_| DEFAULT_CHROME.into());
        let raw =
            RawBrowser::launch_with_switches(std::path::Path::new(&path), &["--site-per-process"])?;
        let b = Browser::connect(raw)?;
        let target = b.create(
            &format!("http://127.0.0.1:{}/", main.port),
            Size {
                width: 800,
                height: 600,
                dpr: 1.,
            },
            true,
        )?;
        let mut d = Driver { b, seq: 0, target };
        for dpr in [1., 2.] {
            if dpr == 2. {
                d.target = d.b.resize(
                    d.target.id,
                    Size {
                        width: 900,
                        height: 650,
                        dpr,
                    },
                )?;
            }
            d.state(|v| v["child"].is_object() && v["dpr"] == dpr)?;
            thread::sleep(Duration::from_millis(300));
            d.send(Input::Focus(true))?;
            d.state(|v| v["focus"] == true)?;
            d.click(50., 30.)?;
            let expected = if dpr == 1. { 1 } else { 2 };
            d.state(|v| v["clicks"] == expected)?;
            println!("testing iframe click at DPR {dpr}");
            d.click(350., 30.)?;
            d.state(|v| v["child"]["clicks"] == expected)?;
            d.click(50., 95.)?;
            d.type_a()?;
            let expected_text = if dpr == 1. { "a" } else { "aa" };
            let state = d.state(|v| {
                v["value"] == expected_text
                    && v["events"]
                        .as_array()
                        .is_some_and(|a| a.len() == 4 * expected as usize)
            })?;
            let expected_events: Vec<&str> = (0..expected)
                .flat_map(|_| ["keydown", "keypress", "input", "keyup"])
                .collect();
            ensure!(
                state["events"] == serde_json::json!(expected_events),
                "Wrong main key sequence: {state}"
            );
            d.click(350., 95.)?;
            d.type_a()?;
            let state = d.state(|v| {
                v["child"]["value"] == expected_text
                    && v["child"]["events"]
                        .as_array()
                        .is_some_and(|a| a.len() == 4 * expected as usize)
            })?;
            ensure!(
                state["child"]["events"] == serde_json::json!(expected_events),
                "Wrong iframe key sequence"
            );
            d.cursor(650., 200., 0)?;
            d.cursor(50., 30., 1)?;
            d.cursor(50., 95., 2)?;
            d.cursor(350., 30., 1)?;
            d.send(Input::Wheel {
                x: 700.,
                y: 400.,
                delta_x: 0.,
                delta_y: 240.,
                buttons: 0,
            })?;
            d.state(|v| v["scroll"].as_f64().unwrap_or_default() > 0.)?;
            d.send(Input::Wheel {
                x: 700.,
                y: 400.,
                delta_x: 0.,
                delta_y: -1000.,
                buttons: 0,
            })?;
            d.state(|v| v["scroll"] == 0)?;
            d.send(Input::Focus(false))?;
            d.state(|v| v["focus"] == false)?;
            d.send(Input::Focus(true))?;
            d.state(|v| v["focus"] == true)?;
            desktop(d.b.pid())?;
            if !foreground.0.is_null() {
                ensure!(
                    unsafe { GetForegroundWindow() } == foreground,
                    "Foreground changed"
                );
            }
            println!("DPR {dpr}: main+cross-origin click, key sequence/text, cursor, focus passed");
        }
        d.send(Input::Wheel {
            x: 700.,
            y: 400.,
            delta_x: 0.,
            delta_y: 240.,
            buttons: 0,
        })?;
        d.state(|v| v["scroll"].as_f64().unwrap_or_default() > 0.)?;
        // A committed multi-codepoint text/IME chunk is inserted once.
        d.b.navigate(
            d.target.id,
            &format!("http://127.0.0.1:{}/fresh", main.port),
        )?;
        d.state(|v| v["value"] == "" && v["scroll"] == 0)?;
        d.click(50., 95.)?;
        d.send(Input::Char("hello\u{1f600}".into()))?;
        d.state(|v| v["value"] == "hello\u{1f600}")?;
        // KEY_DOWN text remains informational even across a held-key blur.
        d.send(Input::KeyDown(Key {
            vk: 65,
            code: "KeyA".into(),
            key: "a".into(),
            text: "a".into(),
            location: 0,
            repeat: false,
            is_system: false,
        }))?;
        d.send(Input::Focus(false))?;
        let state = d.state(|v| {
            v["focus"] == false && v["events"] == serde_json::json!(["input", "keydown", "keyup"])
        })?;
        ensure!(
            state["value"] == "hello\u{1f600}",
            "Held key inserted KEY_DOWN text"
        );
        d.send(Input::Focus(true))?;
        d.send(Input::MouseDown {
            x: 50.,
            y: 95.,
            button: 1,
            buttons: 1,
            click_count: 1,
        })?;
        d.send(Input::Focus(false))?;
        d.state(|v| {
            v["focus"] == false
                && v["mouse"]
                    .as_array()
                    .is_some_and(|events| events.iter().any(|e| e[0] == "mouseup"))
        })?;
        d.send(Input::Focus(true))?;
        println!("Held key/button release on blur passed");
        // Stale resize generation, inactive target and invalid DOM code.
        let old = d.target.clone();
        d.target = d.b.resize(
            old.id,
            Size {
                width: 850,
                height: 620,
                dpr: 2.,
            },
        )?;
        d.seq += 1;
        d.b.input(
            &InputHeader {
                target: old.id,
                generation: old.generation,
                sequence: d.seq,
                modifiers: 0,
            },
            &Input::Focus(true),
        )?;
        d.fault(10)?;
        d.send(Input::KeyDown(Key {
            vk: 65,
            code: "BadCode".into(),
            key: "a".into(),
            text: String::new(),
            location: 0,
            repeat: false,
            is_system: false,
        }))?;
        d.fault(1)?;
        let other = d.b.create(
            "about:blank",
            Size {
                width: 400,
                height: 300,
                dpr: 1.,
            },
            true,
        )?;
        thread::sleep(Duration::from_millis(1100));
        d.send(Input::Focus(true))?;
        d.fault(10)?;
        d.target = d.b.activate(old.id)?;
        // Exercise navigation/close while pointer and key transitions are in flight.
        for _ in 0..15 {
            d.send(Input::MouseMove {
                x: 50.,
                y: 30.,
                buttons: 0,
            })?;
        }
        d.b.navigate(d.target.id, "about:blank")?;
        for _ in 0..15 {
            d.send(Input::MouseMove {
                x: 350.,
                y: 95.,
                buttons: 0,
            })?;
        }
        d.b.close(d.target.id)?;
        thread::sleep(Duration::from_millis(1100));
        d.send(Input::Focus(true))?;
        d.fault(10)?;
        d.b.close(other.id)?;
        d.b.ping(42)?;
        desktop(d.b.pid())?;
        d.b.shutdown()?;
        malformed_inputs()?;
        println!(
            "M2 PASSED: wheel, text commit, resize/stale/invalid/inactive, navigation and close races; foreground status reported separately"
        );
        Ok(())
    }
}
