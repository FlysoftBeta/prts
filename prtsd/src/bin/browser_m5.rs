//! Real-browser M5 gates. No CDP input or desktop input is used.
#[cfg(windows)]
#[path = "support/desktop.rs"]
mod desktop;
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    acceptance::run()
}
#[cfg(not(windows))]
fn main() {
    eprintln!("browser_m5 requires Windows");
}

#[cfg(windows)]
mod acceptance {
    use super::desktop::DesktopProbe;
    use anyhow::{Context, Result, ensure};
    use prtsd::{
        browser::{DEFAULT_CHROME, RawBrowser},
        browser_wire::{self as v1, Record, Size},
        window_wire::*,
    };
    use std::{io::Write, path::Path, sync::mpsc, thread, time::Duration};

    struct Client {
        raw: RawBrowser,
        rx: mpsc::Receiver<Result<Record>>,
        next: u64,
        resources: prtsd::browser_frames::Resources,
        scenes: std::collections::BTreeMap<u64, Scene>,
        composed: prtsd::scene_frames::Scenes,
        sequence: u64,
        faults: Vec<u32>,
        cursors: Vec<(u64, u64, u8)>,
        widget_count: u32,
        menu_button: Option<DipRect>,
        menu_items: Vec<(u32, DipRect)>,
    }
    impl Client {
        fn start() -> Result<Self> {
            let path = std::env::args().nth(1).unwrap_or(DEFAULT_CHROME.into());
            let switches = vec!["--prts-test-hooks"];
            let raw = RawBrowser::launch_with_switches(Path::new(&path), &switches)?;
            raw.wait_startup()?;
            let mut event = raw.event.try_clone()?;
            let (tx, rx) = mpsc::channel();
            let event_tx = tx.clone();
            thread::spawn(move || {
                let mut major = 1;
                loop {
                    let record = Record::read_version(&mut event, major);
                    if record.as_ref().is_ok_and(|r| r.kind == v1::HELLO_ACK) {
                        major = 2;
                    }
                    let failed = record.is_err();
                    if event_tx.send(record).is_err() || failed {
                        break;
                    }
                }
            });
            let mut frame = raw.frame.try_clone()?;
            let frame_tx = tx.clone();
            thread::spawn(move || {
                loop {
                    let r = Record::read_version(&mut frame, 2);
                    let failed = r.is_err();
                    if failed || frame_tx.send(r).is_err() {
                        break;
                    }
                }
            });
            let luid = if std::env::var("PRTS_FRAME_MODE").as_deref() == Ok("cpu") {
                0
            } else {
                prtsd::d3d::shared()?.lock().unwrap().luid
            };
            let mut resources = prtsd::browser_frames::Resources::default();
            resources.set_gpu_adapter(luid);
            resources.set_layer_alpha(true);
            let mut c = Self {
                raw,
                rx,
                next: 1,
                resources,
                scenes: Default::default(),
                composed: Default::default(),
                sequence: 0,
                faults: vec![],
                cursors: vec![],
                widget_count: 0,
                menu_button: None,
                menu_items: Vec::new(),
            };
            let inflight = if std::env::var_os("PRTS_M5_ONE_CREDIT").is_some() {
                1
            } else {
                2
            };
            hello(1, luid, if luid != 0 { 3 } else { 2 }, inflight).write(&mut c.raw.command)?;
            let ack = c.receive()?;
            check_ack(&ack, 1)?;
            let caps = c.receive()?;
            ensure!(
                caps.kind == WINDOW_CAPS && caps.id == 0,
                "Caps must be first v2 notification"
            );
            println!("PASS exact-v2 negotiation, bootstrap ACK then v2 caps");
            Ok(c)
        }
        fn receive(&self) -> Result<Record> {
            self.rx
                .recv_timeout(Duration::from_secs(5))
                .context("v2 response timeout")?
        }
        fn request(&mut self, kind: u16, payload: Vec<u8>) -> Result<Record> {
            self.next += 2;
            self.raw.command.write_all(
                &Record {
                    kind,
                    id: self.next,
                    payload,
                }
                .encode_version(2)?,
            )?;
            loop {
                let r = self.receive()?;
                if r.id == self.next {
                    return Ok(r);
                }
                self.consume(r)?;
            }
        }
        fn consume(&mut self, r: Record) -> Result<()> {
            if r.kind == v1::FAULT {
                if let v1::Event::Fault {
                    message,
                    fatal,
                    code,
                } = r.event()?
                {
                    self.faults.push(code);
                    println!("WINDOW FAULT fatal={fatal}: {message}");
                    ensure!(!fatal, "Fatal window fault {message}");
                }
                return Ok(());
            }
            if matches!(r.kind, 0x300 | 0x301 | 0x310 | 0x311) {
                let reply = match self.resources.process(&r, self.raw.process_handle()) {
                    Ok(reply) => reply,
                    Err(e) if r.kind == 0x300 && r.payload.get(24) == Some(&1) => {
                        println!("GPU import rejected; exercising M4 CPU fallback: {e:#}");
                        let mut payload = 9u32.to_le_bytes().to_vec();
                        v1::string("GPU import rejected; CPU fallback", &mut payload)?;
                        payload.push(0);
                        Record {
                            kind: v1::ERROR,
                            id: r.id,
                            payload,
                        }
                    }
                    Err(e) => return Err(e).context("Processing v2 M4 resource/descriptor"),
                };
                self.raw.command.write_all(&reply.encode_version(2)?)?;
                while let Some(frame) = self.resources.take_frame() {
                    self.composed.push(frame)?;
                }
                return Ok(());
            }
            ensure!(
                r.id == 0
                    && matches!(
                        r.kind,
                        WINDOW_CREATED
                            | WINDOW_CHANGED
                            | WINDOW_CLOSED
                            | WINDOW_SCENE
                            | WINDOW_CURSOR
                    ),
                "Unexpected v2 notification {r:?}"
            );
            if matches!(r.kind, WINDOW_CREATED | WINDOW_CHANGED) {
                Window::decode(&r.payload)?;
            }
            if r.kind == WINDOW_SCENE {
                let scene = Scene::decode(&r.payload)?;
                if let Some(previous) = self.scenes.get(&scene.window) {
                    ensure!(
                        scene.revision > previous.revision,
                        "Scene revision regressed"
                    );
                }
                self.composed.update(scene.clone())?;
                self.scenes.insert(scene.window, scene);
            }
            if r.kind == WINDOW_CURSOR {
                let converted = Record {
                    kind: v1::CURSOR_CHANGED,
                    id: 0,
                    payload: r.payload,
                };
                if let v1::Event::Cursor {
                    target,
                    generation,
                    cursor,
                } = converted.event()?
                {
                    self.cursors.push((target, generation, cursor.shape));
                }
            }
            Ok(())
        }
        fn list(&mut self) -> Result<Vec<Window>> {
            let r = self.request(WINDOW_LIST, vec![])?;
            ensure!(r.kind == WINDOW_LIST_RESULT, "List failed {r:?}");
            Window::list(&r.payload)
        }
        fn create(&mut self, url: &str) -> Result<Window> {
            let mut p = vec![];
            v1::string(url, &mut p)?;
            Size {
                width: 960,
                height: 640,
                dpr: 1.,
            }
            .encode(&mut p)?;
            p.push(1);
            let r = self.request(WINDOW_CREATE, p)?;
            ensure!(r.kind == WINDOW_RESULT, "Create failed {r:?}");
            Window::decode(&r.payload)
        }
        fn tab(&mut self, owner: u64, url: &str) -> Result<Tab> {
            let mut p = owner.to_le_bytes().to_vec();
            v1::string(url, &mut p)?;
            p.push(1);
            let r = self.request(TAB_CREATE, p)?;
            ensure!(r.kind == TAB_RESULT, "Tab create failed {r:?}");
            Tab::decode(&r.payload)
        }
        fn expect_error(&mut self, kind: u16, payload: Vec<u8>) -> Result<()> {
            let r = self.request(kind, payload)?;
            ensure!(r.kind == v1::ERROR, "Invalid operation accepted {r:?}");
            Ok(())
        }
        fn frame(&mut self, window: u64) -> Result<prtsd::browser_frames::CpuFrame> {
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(f) = self.composed.take_window(window)? {
                    return Ok(f);
                }
                ensure!(
                    std::time::Instant::now() < deadline,
                    "Window frame timeout; scenes={:?}",
                    self.scenes
                );
                match self.rx.recv_timeout(Duration::from_millis(10)) {
                    Ok(record) => self.consume(record?)?,
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        fn geometry(&mut self, id: u64) -> Result<(u32, u32, f64, Vec<DipRect>)> {
            let r = self.request(0x42d, id.to_le_bytes().to_vec())?;
            ensure!(r.kind == 0x48f, "Geometry probe failed {r:?}");
            let mut d = v1::Decoder::new(&r.payload);
            let width = d.u32()?;
            let height = d.u32()?;
            let dsf = f64::from_bits(d.u64()?);
            let mut rects = vec![];
            for _ in 0..4 {
                rects.push(read_rect(&mut d)?);
            }
            let tabs = d.u32()?;
            for _ in 0..tabs {
                rects.push(read_rect(&mut d)?);
            }
            ensure!(d.boolean()?, "Native headless input guards are disabled");
            let blocked = d.u64()?;
            ensure!(d.u64()? == 0, "Browser wrote native OS cursor state");
            self.widget_count = d.u32()?;
            let address = d.string()?;
            let focused = d.string()?;
            let active = d.boolean()?;
            println!(
                "VIEWS input diagnostic widgets={} focused={focused} active={active} omnibox={address:?}",
                self.widget_count
            );
            println!(
                "NATIVE cursor guards enabled, blocked calls={blocked}; no native cursor writes permitted"
            );
            d.finish()?;
            Ok((width, height, dsf, rects))
        }
    }
    fn read_rect(d: &mut v1::Decoder<'_>) -> Result<DipRect> {
        Ok(DipRect {
            x: f64::from_bits(d.u64()?),
            y: f64::from_bits(d.u64()?),
            width: f64::from_bits(d.u64()?),
            height: f64::from_bits(d.u64()?),
        })
    }
    include!("support/m5_capture.rs");
    include!("support/m5_input.rs");
    include!("support/m5_hosts.rs");
    include!("support/m5_flyouts.rs");
    include!("support/m5_context.rs");
    pub fn run() -> Result<()> {
        let desktop = DesktopProbe::start()?;
        let mut c = Client::start()?;
        ensure!(c.list()?.is_empty(), "Startup browser leaked into registry");
        let a = c.create("about:blank")?;
        let b = c.create("about:blank")?;
        let t1 = c.tab(a.id, "data:text/html,one")?;
        let t2 = c.tab(a.id, "data:text/html,two")?;
        let t3 = c.tab(b.id, "data:text/html,three")?;
        let all = c.list()?;
        ensure!(
            all.len() == 2 && all.iter().map(|w| w.tabs.len()).sum::<usize>() == 5,
            "Window/tab enumeration {all:?}"
        );
        ensure!(
            all.iter().all(|w| w.generation == 1),
            "Tab commands changed window generation"
        );
        println!(
            "PASS two Browsers / five tabs, independent selection and stable window generations"
        );
        c.expect_error(WINDOW_ACTIVATE, t1.id.to_le_bytes().to_vec())?;
        c.expect_error(TAB_ACTIVATE, a.id.to_le_bytes().to_vec())?;
        c.expect_error(WINDOW_CLOSE, u64::MAX.to_le_bytes().to_vec())?;
        c.expect_error(WINDOW_LIST, vec![0])?;
        c.expect_error(WINDOW_CREATE, vec![0; 20])?;
        c.expect_error(v1::CREATE, vec![])?;
        println!(
            "PASS unknown, wrong-kind/cross-owner IDs, malformed commands, v1 command isolation"
        );
        let mut moved = t2.id.to_le_bytes().to_vec();
        moved.extend(b.id.to_le_bytes());
        ensure!(c.request(0x42e, moved)?.kind == v1::OK, "Move failed");
        let all = c.list()?;
        ensure!(
            all.iter()
                .find(|w| w.id == b.id)
                .context("Missing B")?
                .tabs
                .iter()
                .any(|t| t.id == t2.id && t.window == b.id),
            "Moved tab lost identity"
        );
        ensure!(
            c.request(0x42f, t1.id.to_le_bytes().to_vec())?.kind == v1::OK,
            "Replace failed"
        );
        ensure!(
            c.list()?
                .iter()
                .find(|w| w.id == a.id)
                .context("Missing A")?
                .tabs
                .iter()
                .any(|t| t.id == t1.id),
            "Replacement lost identity"
        );
        println!("PASS real detach/reinsert and WebContents discard preserve tab identity");
        ensure!(
            c.request(TAB_CLOSE, t3.id.to_le_bytes().to_vec())?.kind == v1::OK,
            "Tab close failed"
        );
        c.expect_error(TAB_ACTIVATE, t3.id.to_le_bytes().to_vec())?;
        for id in [a.id, b.id] {
            ensure!(
                c.request(WINDOW_CLOSE, id.to_le_bytes().to_vec())?.kind == v1::OK,
                "Close rejected"
            );
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !c.list()?.is_empty() {
            ensure!(
                std::time::Instant::now() < deadline,
                "Window close did not complete"
            );
        }
        c.expect_error(WINDOW_ACTIVATE, a.id.to_le_bytes().to_vec())?;
        let recreated = c.create("about:blank")?;
        ensure!(
            recreated.id > t3.id,
            "Reused identity after zero-window recreation"
        );
        println!(
            "PASS stale IDs, accepted close, zero-window recreation with monotonic identities"
        );
        if std::env::var_os("PRTS_M5_CAPTURE").is_some() {
            capture_gate(&mut c, recreated.clone())?;
        }
        if std::env::var_os("PRTS_M5_INPUT").is_some() {
            input_gate(&mut c, recreated.id)?;
        }
        if std::env::var_os("PRTS_M5_HOSTS").is_some() {
            host_gate(&mut c, recreated.id)?;
        }
        if std::env::var_os("PRTS_M5_FLYOUTS").is_some() {
            let trace = std::env::var_os("PRTS_M5_TRACE").is_some();
            let trace_request = |start: u8| {
                let mut payload = vec![start];
                v1::string("C:/src/chromium/prts-flyouts-copy.json", &mut payload)
                    .expect("literal trace path");
                payload
            };
            if trace {
                c.request(0x429, trace_request(1))?;
            }
            let result = flyout_gate(&mut c, recreated.id, &desktop);
            if trace {
                c.request(0x429, trace_request(0))?;
            }
            result?;
        }
        if std::env::var_os("PRTS_M5_CONTEXT").is_some() {
            context_gate(&mut c, recreated.id, &desktop)?;
        }
        for window in c.list()? {
            let mut p = window.id.to_le_bytes().to_vec();
            p.push(0);
            p.extend(0u32.to_le_bytes());
            p.extend(0u32.to_le_bytes());
            ensure!(
                c.request(WINDOW_FRAME_CONFIG, p)?.kind == WINDOW_RESULT,
                "Stop capture failed"
            );
        }
        while c.resources.resource_count() > 0 {
            let r = c.receive()?;
            c.consume(r)?;
        }
        println!(
            "PASS v2 frame producers stopped and registered resources fully drained before shutdown"
        );
        desktop.check(c.raw.pid, "M5 registry")?;
        ensure!(
            c.request(v1::SHUTDOWN, vec![])?.kind == v1::OK,
            "Shutdown failed"
        );
        ensure!(c.raw.wait(Duration::from_secs(5)), "Shutdown hung");
        println!("PASS M5 step1 registry acceptance and desktop isolation");
        Ok(())
    }
}
