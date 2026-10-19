//! Run after building patched chrome: cargo run --bin browser_m1 -- [chrome.exe]
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    acceptance::run()
}
#[cfg(not(windows))]
fn main() {
    eprintln!("browser_m1 requires Windows");
}

#[cfg(windows)]
mod acceptance {
    use anyhow::{Context, Result, ensure};
    use prtsd::{
        browser::{Browser, DEFAULT_CHROME, RawBrowser, startup_timeout},
        browser_wire::*,
    };
    use std::{
        collections::{BTreeSet, HashSet},
        io::{BufRead, Write},
        path::Path,
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, Ordering},
        },
        thread,
        time::{Duration, Instant},
    };
    use windows::{
        Win32::{
            Foundation::{CloseHandle, HWND, LPARAM, WAIT_OBJECT_0},
            Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
            System::{
                Com::{
                    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                    CoUninitialize,
                },
                Threading::{
                    AttachThreadInput, GetCurrentThreadId, OpenProcess, PROCESS_SYNCHRONIZE,
                    WaitForSingleObject,
                },
            },
            UI::{
                Accessibility::{
                    CUIAutomation, IUIAutomation, TreeScope_Descendants, UIA_ButtonControlTypeId,
                },
                WindowsAndMessaging::*,
            },
        },
        core::BOOL,
    };

    #[derive(Clone, Debug)]
    struct Window {
        hwnd: usize,
        pid: u32,
        visible: bool,
        cloaked: u32,
        style: isize,
        ex_style: isize,
        title: String,
        class: String,
    }
    unsafe extern "system" fn enumerate(hwnd: HWND, param: LPARAM) -> BOOL {
        let windows = unsafe { &mut *(param.0 as *mut Vec<Window>) };
        let mut pid = 0;
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }
        let mut cloaked = 0u32;
        let cloak = unsafe {
            DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, (&mut cloaked as *mut u32).cast(), 4)
        };
        let mut title = vec![0u16; 1024];
        let n = unsafe { GetWindowTextW(hwnd, &mut title) };
        let mut class = vec![0u16; 256];
        let class_n = unsafe { GetClassNameW(hwnd, &mut class) };
        windows.push(Window {
            hwnd: hwnd.0 as usize,
            pid,
            visible: unsafe { IsWindowVisible(hwnd).as_bool() },
            cloaked: if cloak.is_ok() { cloaked } else { 0 },
            style: unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) },
            ex_style: unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) },
            title: String::from_utf16_lossy(&title[..n as usize]),
            class: String::from_utf16_lossy(&class[..class_n as usize]),
        });
        true.into()
    }
    fn windows() -> Result<Vec<Window>> {
        let mut list = Vec::new();
        unsafe {
            EnumWindows(
                Some(enumerate),
                LPARAM((&mut list as *mut Vec<Window>) as isize),
            )?;
        }
        Ok(list)
    }
    fn taskbar_buttons() -> Result<BTreeSet<String>> {
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        }
        struct Com;
        impl Drop for Com {
            fn drop(&mut self) {
                unsafe {
                    CoUninitialize();
                }
            }
        }
        let _com = Com;
        let automation: IUIAutomation =
            unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }?;
        let condition = unsafe { automation.CreateTrueCondition() }?;
        let mut buttons = BTreeSet::new();
        let mut trays = 0;
        for window in windows()?
            .iter()
            .filter(|w| w.class == "Shell_TrayWnd" || w.class == "Shell_SecondaryTrayWnd")
        {
            trays += 1;
            let root = unsafe { automation.ElementFromHandle(HWND(window.hwnd as *mut _)) }?;
            let elements = unsafe { root.FindAll(TreeScope_Descendants, &condition) }?;
            for i in 0..unsafe { elements.Length() }? {
                let element = unsafe { elements.GetElement(i) }?;
                let name = unsafe { element.CurrentName() }?.to_string();
                let class = unsafe { element.CurrentClassName() }?.to_string();
                let lower = name.to_ascii_lowercase();
                if (unsafe { element.CurrentControlType() }? == UIA_ButtonControlTypeId
                    && class.to_ascii_lowercase().contains("task"))
                    || lower.contains("chromium")
                    || lower.contains("google chrome")
                    || lower.contains("prts-m1-navigation")
                {
                    buttons.insert(format!("{class}: {name}"));
                }
            }
        }
        ensure!(
            trays > 0,
            "Explorer taskbar HWND not found for taskbar visibility check"
        );
        Ok(buttons)
    }
    struct NotepadSentinel {
        child: std::process::Child,
        file: std::path::PathBuf,
    }
    impl Drop for NotepadSentinel {
        fn drop(&mut self) {
            if let Some(token) = self.file.file_name()
                && let Ok(list) = windows()
            {
                for window in list
                    .iter()
                    .filter(|w| w.title.contains(token.to_string_lossy().as_ref()))
                {
                    unsafe {
                        let _ = PostMessageW(
                            Some(HWND(window.hwnd as *mut _)),
                            WM_CLOSE,
                            Default::default(),
                            Default::default(),
                        );
                    }
                }
            }
            let _ = self.child.try_wait();
            let _ = std::fs::remove_file(&self.file);
        }
    }
    struct DesktopProbe {
        stop: Arc<AtomicBool>,
        sampler: Option<thread::JoinHandle<()>>,
        changes: Arc<Mutex<Vec<usize>>>,
        sentinel: usize,
        focus_verified: bool,
        _notepad: NotepadSentinel,
        baseline: HashSet<usize>,
        taskbar_baseline: BTreeSet<String>,
    }
    impl DesktopProbe {
        fn start() -> Result<Self> {
            let file =
                std::env::temp_dir().join(format!("prts-m1-focus-{}.txt", std::process::id()));
            std::fs::write(
                &file,
                b"PRTS M1 focus sentinel. No input is sent to this application.\r\n",
            )?;
            let notepad = std::process::Command::new("notepad.exe")
                .arg(&file)
                .spawn()?;
            let notepad = NotepadSentinel {
                child: notepad,
                file,
            };
            let token = notepad.file.file_name().unwrap().to_string_lossy();
            let until = Instant::now() + Duration::from_secs(10);
            let sentinel = loop {
                if let Some(w) = windows()?
                    .iter()
                    .find(|w| w.visible && w.title.contains(token.as_ref()))
                {
                    break w.hwnd;
                }
                ensure!(
                    Instant::now() < until,
                    "Notepad focus sentinel HWND not found"
                );
                thread::sleep(Duration::from_millis(100));
            };
            unsafe {
                let foreground = GetForegroundWindow();
                let foreground_thread = GetWindowThreadProcessId(foreground, None);
                let current_thread = GetCurrentThreadId();
                let attached = foreground_thread != 0
                    && foreground_thread != current_thread
                    && AttachThreadInput(current_thread, foreground_thread, true).as_bool();
                let _ = ShowWindow(HWND(sentinel as *mut _), SW_RESTORE);
                let _ = SetForegroundWindow(HWND(sentinel as *mut _));
                if attached {
                    let _ = AttachThreadInput(current_thread, foreground_thread, false);
                }
            }
            thread::sleep(Duration::from_millis(300));
            let foreground = unsafe { GetForegroundWindow().0 as usize };
            let focus_verified = foreground == sentinel;
            ensure!(
                focus_verified
                    || (foreground == 0
                        && std::env::var("PRTS_M1_ALLOW_DISCONNECTED_DESKTOP").as_deref()
                            == Ok("1")),
                "Could not establish Notepad foreground focus (wanted0x{sentinel:X},got0x{foreground:X}); reconnect the Windows session or explicitly set PRTS_M1_ALLOW_DISCONNECTED_DESKTOP=1 to mark this check UNVERIFIED"
            );
            if !focus_verified {
                println!(
                    "DESKTOP foreground check UNVERIFIED: GetForegroundWindow=NULL in disconnected Windows session; no interactive focus baseline exists"
                );
            }
            let expected_foreground = foreground;
            let baseline = windows()?.iter().map(|w| w.hwnd).collect();
            let taskbar_baseline = taskbar_buttons()?;
            println!("DESKTOP Explorer UIAutomation taskbar baseline: {taskbar_baseline:?}");
            let stop = Arc::new(AtomicBool::new(false));
            let changes = Arc::new(Mutex::new(Vec::new()));
            let s = stop.clone();
            let c = changes.clone();
            let sampler = thread::spawn(move || {
                while !s.load(Ordering::Acquire) {
                    let fg = unsafe { GetForegroundWindow().0 as usize };
                    if fg != expected_foreground {
                        let mut v = c.lock().unwrap();
                        if v.last() != Some(&fg) {
                            v.push(fg);
                        }
                    }
                    thread::sleep(Duration::from_millis(10));
                }
            });
            println!("DESKTOP sentinel=Notepad hwnd=0x{sentinel:X}; foreground sampled every 10ms");
            Ok(Self {
                stop,
                sampler: Some(sampler),
                changes,
                sentinel,
                focus_verified,
                _notepad: notepad,
                baseline,
                taskbar_baseline,
            })
        }
        fn check(&self, pid: u32, stage: &str) -> Result<()> {
            let snapshot = windows()?;
            let owned: Vec<_> = snapshot.iter().filter(|w| w.pid == pid).collect();
            for w in &owned {
                println!(
                    "DESKTOP {stage}: hwnd=0x{:X} visible={} DWMWA_CLOAKED={} WS_STYLE=0x{:X} WS_EXSTYLE=0x{:X} title={:?}",
                    w.hwnd, w.visible, w.cloaked, w.style, w.ex_style, w.title
                );
                ensure!(
                    !w.visible || w.cloaked != 0,
                    "Visible uncloaked PRTS top-level window"
                );
            }
            let new_visible: Vec<_> = snapshot
                .iter()
                .filter(|w| {
                    !self.baseline.contains(&w.hwnd) && w.pid == pid && w.visible && w.cloaked == 0
                })
                .collect();
            ensure!(new_visible.is_empty(), "New visible browser windows");
            let taskbar = taskbar_buttons()?;
            let added: Vec<_> = taskbar.difference(&self.taskbar_baseline).collect();
            ensure!(
                added.is_empty(),
                "New or changed Explorer taskbar browser/running-app buttons: {added:?}"
            );
            println!(
                "DESKTOP {stage}: Explorer UIAutomation taskbar buttons unchanged ({taskbar:?})"
            );
            ensure!(
                unsafe { GetForegroundWindow().0 as usize }
                    == if self.focus_verified {
                        self.sentinel
                    } else {
                        0
                    },
                "Foreground changed at {stage}"
            );
            let changes = self.changes.lock().unwrap().clone();
            ensure!(
                changes.is_empty(),
                "Foreground changed during PRTS operation: {changes:?}"
            );
            println!(
                "DESKTOP {stage}: {} browser HWNDs, 0 visible/uncloaked, 0 visible taskbar-eligible windows; foreground {}",
                owned.len(),
                if self.focus_verified {
                    "unchanged"
                } else {
                    "UNVERIFIED (disconnected desktop)"
                }
            );
            Ok(())
        }
    }
    impl Drop for DesktopProbe {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(h) = self.sampler.take() {
                let _ = h.join();
            }
        }
    }
    fn ordered(browser: &Browser, start: usize, event: u16, response: u16) -> Result<()> {
        let t = browser.transcript();
        let new = &t[start..];
        let e = new
            .iter()
            .position(|v| v.0 == event)
            .context("Required mutation event missing")?;
        let r = new
            .iter()
            .position(|v| v.0 == response)
            .context("Mutation response missing")?;
        ensure!(e < r, "Mutation response preceded event: {new:?}");
        Ok(())
    }
    fn collect_until(
        browser: &Browser,
        all: &mut Vec<Event>,
        predicate: impl Fn(&Event) -> bool,
    ) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            all.extend(browser.drain_events());
            if all.iter().any(&predicate) {
                return Ok(());
            }
            ensure!(
                Instant::now() < deadline,
                "Expected lifecycle event timed out: {all:?}"
            );
            if let Some(event) = browser.receive_event(Duration::from_millis(250))? {
                all.push(event);
            }
        }
    }
    fn hello() -> Record {
        let mut p = Vec::new();
        for n in [1u16, 0, 1, 0] {
            p.extend(n.to_le_bytes());
        }
        p.extend(std::process::id().to_le_bytes());
        p.extend(0u64.to_le_bytes());
        p.extend(2u32.to_le_bytes());
        p.extend(3u32.to_le_bytes());
        Record {
            kind: HELLO,
            id: 1,
            payload: p,
        }
    }
    fn receive(raw: &mut RawBrowser) -> Result<Record> {
        let mut file = raw.event.try_clone()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let reader = thread::spawn(move || {
            let _ = tx.send(Record::read(&mut file));
        });
        let result = rx.recv_timeout(Duration::from_secs(5));
        if result.is_err() {
            raw.terminate();
        }
        let _ = reader.join();
        result.context("Raw pipe response deadline")?
    }
    fn ready(path: &Path) -> Result<RawBrowser> {
        let raw = RawBrowser::launch(path)?;
        raw.wait_startup()?;
        Ok(raw)
    }
    fn failure_cases(path: &Path) -> Result<()> {
        // Valid HELLO split across prefix/header/payload boundaries, then protocol shutdown.
        let mut raw = ready(path)?;
        let record = hello().encode()?;
        for chunk in record.chunks(3) {
            raw.command.write_all(chunk)?;
            thread::sleep(Duration::from_millis(2));
        }
        ensure!(
            receive(&mut raw)?.kind == HELLO_ACK,
            "Fragmented HELLO rejected"
        );
        Record {
            kind: SHUTDOWN,
            id: 3,
            payload: vec![],
        }
        .write(&mut raw.command)?;
        ensure!(receive(&mut raw)?.kind == OK, "Raw shutdown did not ACK");
        ensure!(raw.wait(Duration::from_secs(5)), "Raw shutdown hung");
        println!("PASS fragmented valid HELLO and clean shutdown");
        let mut raw = ready(path)?;
        raw.command.write_all(&[48, 0, 0, 0, b'P', b'R'])?;
        let command = std::mem::replace(&mut raw.command, std::fs::File::open("NUL")?);
        drop(command);
        ensure!(
            raw.wait(Duration::from_secs(10)),
            "Partial record + EOF hung browser"
        );
        println!("PASS truncated record/abrupt EOF exits within 10s");
        let mut raw = ready(path)?;
        hello().write(&mut raw.command)?;
        ensure!(
            receive(&mut raw)?.kind == HELLO_ACK,
            "EOF case HELLO failed"
        );
        let command = std::mem::replace(&mut raw.command, std::fs::File::open("NUL")?);
        drop(command);
        ensure!(raw.wait(Duration::from_secs(10)), "Peer EOF hung browser");
        println!("PASS peer command EOF after handshake exits within 10s");
        let mut peer = std::process::Command::new(std::env::current_exe()?)
            .arg("--peer-child")
            .arg(path)
            .stdout(std::process::Stdio::piped())
            .spawn()?;
        let mut line = String::new();
        std::io::BufReader::new(peer.stdout.take().context("Missing peer stdout")?)
            .read_line(&mut line)?;
        let pid: u32 = line
            .trim()
            .strip_prefix("PEER_PID=")
            .context("Peer launcher did not report browser PID")?
            .parse()?;
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }?;
        let peer_status = peer.wait()?;
        let exited = unsafe { WaitForSingleObject(process, 10000) } == WAIT_OBJECT_0;
        unsafe {
            CloseHandle(process)?;
        }
        ensure!(
            peer_status.success() && exited,
            "Browser did not exit after launcher process death"
        );
        println!("PASS abrupt launcher process death closes pipes and browser exits within 10s");
        Ok(())
    }
    fn malformed_cases(path: &Path) -> Result<()> {
        let raw = RawBrowser::launch_configuration(path, &[], false)?;
        ensure!(
            raw.wait(startup_timeout()?) && raw.exit_code()? != 0,
            "Nonheadless PRTS bootstrap did not reject before GUI startup"
        );
        println!("PASS PRTS bootstrap rejects missing headless before window startup");
        for (label, switches) in [
            ("missing key", vec!["--prts-pipe=cmd:0x1,evt:0x2"]),
            (
                "duplicate key",
                vec!["--prts-pipe=cmd:0x1,evt:0x2,cmd:0x3,frame:0x4"],
            ),
            (
                "null handles",
                vec!["--prts-pipe=cmd:0x0,evt:0x0,frame:0x0"],
            ),
            (
                "invalid hexadecimal",
                vec!["--prts-pipe=cmd:0xG,evt:0x2,frame:0x3"],
            ),
            (
                "pointer overflow",
                vec!["--prts-pipe=cmd:0x10000000000000000,evt:0x2,frame:0x3"],
            ),
            ("renderer process switch", vec!["--type=renderer"]),
            ("GPU process switch", vec!["--type=gpu-process"]),
            ("old headless mode", vec!["--headless=old"]),
            ("empty headless value", vec!["--headless"]),
        ] {
            let raw = RawBrowser::launch_with_switches(path, &switches)?;
            ensure!(
                raw.wait(startup_timeout()?),
                "Bootstrap case {label} did not reject/exit"
            );
            ensure!(
                raw.exit_code()? != 0,
                "Bootstrap case {label} exited successfully instead of rejection"
            );
            println!("PASS bootstrap rejects {label}");
        }
        for label in [
            "unknown type",
            "bad magic",
            "bad version",
            "nonzero flags",
            "even request ID",
            "oversized prefix",
            "short envelope",
        ] {
            let mut raw = ready(path)?;
            let mut bytes = hello().encode()?;
            match label {
                "unknown type" => bytes[12..14].copy_from_slice(&0x9999u16.to_le_bytes()),
                "bad magic" => bytes[4] = 0,
                "bad version" => bytes[8..10].copy_from_slice(&2u16.to_le_bytes()),
                "nonzero flags" => bytes[14] = 1,
                "even request ID" => bytes[16..24].copy_from_slice(&2u64.to_le_bytes()),
                "oversized prefix" => bytes = (1_048_577u32).to_le_bytes().to_vec(),
                "short envelope" => bytes = 19u32.to_le_bytes().to_vec(),
                _ => unreachable!(),
            };
            raw.command.write_all(&bytes)?;
            ensure!(
                raw.wait(Duration::from_secs(10)),
                "Malformed wire {label} did not fail/exit"
            );
            println!("PASS malformed wire rejects {label}");
        }
        let mut raw = ready(path)?;
        let mut record = hello();
        record.payload[..8].copy_from_slice(&[2, 0, 0, 0, 2, 0, 0, 0]);
        record.write(&mut raw.command)?;
        let response = receive(&mut raw)?;
        ensure!(
            response.kind == ERROR && response.id == 1,
            "No-overlap HELLO did not return correlated ERROR"
        );
        let mut d = Decoder::new(&response.payload);
        ensure!(d.u32()? == 3, "No-overlap HELLO error must be unsupported");
        let _ = d.string()?;
        let _ = d.boolean()?;
        d.finish()?;
        ensure!(
            raw.wait(Duration::from_secs(10)),
            "No-overlap HELLO did not exit"
        );
        println!("PASS version negotiation no-overlap ERROR then EOF");
        for label in ["reused ID", "decreasing odd ID", "wrong direction"] {
            let mut raw = ready(path)?;
            hello().write(&mut raw.command)?;
            ensure!(
                receive(&mut raw)?.kind == HELLO_ACK,
                "Negative request HELLO failed"
            );
            let record = match label {
                "reused ID" => Record {
                    kind: PING,
                    id: 1,
                    payload: 0u64.to_le_bytes().to_vec(),
                },
                "decreasing odd ID" => {
                    Record {
                        kind: PING,
                        id: 5,
                        payload: 0u64.to_le_bytes().to_vec(),
                    }
                    .write(&mut raw.command)?;
                    ensure!(receive(&mut raw)?.kind == PONG, "First PING failed");
                    Record {
                        kind: PING,
                        id: 3,
                        payload: 0u64.to_le_bytes().to_vec(),
                    }
                }
                _ => Record {
                    kind: PONG,
                    id: 3,
                    payload: 0u64.to_le_bytes().to_vec(),
                },
            };
            record.write(&mut raw.command)?;
            ensure!(
                raw.wait(Duration::from_secs(10)),
                "Invalid request {label} did not fail/exit"
            );
            println!("PASS protocol rejects {label}");
        }
        let raw = ready(path)?;
        ensure!(
            raw.wait(Duration::from_secs(8)),
            "Missing HELLO did not enforce 5s deadline"
        );
        println!("PASS missing HELLO deadline");
        let mut raw = ready(path)?;
        hello().write(&mut raw.command)?;
        ensure!(
            receive(&mut raw)?.kind == HELLO_ACK,
            "Stalled writer HELLO failed"
        );
        let mut command = raw.command.try_clone()?;
        let writer = thread::spawn(move || {
            for i in 0..20_000u64 {
                if (Record {
                    kind: PING,
                    id: 3 + i * 2,
                    payload: i.to_le_bytes().to_vec(),
                })
                .write(&mut command)
                .is_err()
                {
                    return;
                }
            }
            let _ = Record {
                kind: SHUTDOWN,
                id: 40_003,
                payload: vec![],
            }
            .write(&mut command);
        });
        let exited = raw.wait(Duration::from_secs(15));
        if !exited {
            raw.terminate();
        }
        let _ = writer.join();
        ensure!(
            exited,
            "Stalled event reader blocked browser shutdown/failure"
        );
        println!("PASS stalled event reader/queue pressure cannot hang browser shutdown");
        Ok(())
    }
    fn popup_case(path: &Path, desktop: &DesktopProbe) -> Result<()> {
        // Qualification override only: default launcher keeps Chrome's popup policy intact.
        let browser = Browser::connect(RawBrowser::launch_with_switches(
            path,
            &["--disable-popup-blocking"],
        )?)?;
        let opener=browser.create("data:text/html,<title>PRTS-M1-Opener</title><script>window.open('about:blank','prts-m1-popup')</script>",Size{width:640,height:480,dpr:1.},true)?;
        let mut events = browser.drain_events();
        collect_until(
            &browser,
            &mut events,
            |e| matches!(e,Event::Created(t) if t.kind==2&&t.opener==opener.id),
        )?;
        let popup = events
            .iter()
            .find_map(|e| match e {
                Event::Created(t) if t.kind == 2 && t.opener == opener.id => Some(t),
                _ => None,
            })
            .unwrap();
        ensure!(!popup.active, "Popup stole logical activation");
        ensure!(browser.list()?.len() == 2, "Popup registry mismatch");
        desktop.check(browser.pid(), "allowed popup")?;
        browser.close(popup.id)?;
        browser.close(opener.id)?;
        browser.shutdown()?;
        println!(
            "PASS allowed popup discovery kind=2/opener metadata/nonactivation (test-only --disable-popup-blocking)"
        );
        Ok(())
    }
    pub fn run() -> Result<()> {
        if std::env::args().nth(1).as_deref() == Some("--extended-only") {
            let path = std::env::args()
                .nth(2)
                .unwrap_or_else(|| DEFAULT_CHROME.into());
            malformed_cases(Path::new(&path))?;
            println!("PASS extended bootstrap/framing/version/request-ID/backpressure acceptance");
            return Ok(());
        }
        if std::env::args().nth(1).as_deref() == Some("--raw-startup-probe") {
            let path = std::env::args()
                .nth(2)
                .unwrap_or_else(|| DEFAULT_CHROME.into());
            let mut raw = RawBrowser::launch(Path::new(&path))?;
            println!("RAW_STARTUP launched pid={}", raw.pid);
            hello().write(&mut raw.command)?;
            let mut event = raw.event.try_clone()?;
            let (tx, rx) = std::sync::mpsc::channel();
            let reader = thread::spawn(move || {
                let _ = tx.send(Record::read(&mut event));
            });
            let started = Instant::now();
            let response = loop {
                match rx.recv_timeout(Duration::from_secs(1)) {
                    Ok(result) => break result,
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        println!(
                            "RAW_STARTUP elapsed={:.2}s alive={} exit_code={}",
                            started.elapsed().as_secs_f64(),
                            !raw.wait(Duration::ZERO),
                            raw.exit_code()?
                        );
                        if started.elapsed() >= startup_timeout()? {
                            raw.terminate();
                            break Err(anyhow::anyhow!(
                                "Raw startup exceeded diagnostic startup deadline"
                            ));
                        }
                    }
                    Err(e) => break Err(e.into()),
                }
            };
            let _ = reader.join();
            let response = response?;
            println!(
                "RAW_STARTUP response kind={} id={} after{:.2}s",
                response.kind,
                response.id,
                started.elapsed().as_secs_f64()
            );
            ensure!(response.kind == HELLO_ACK, "Raw startup did not ACK");
            Record {
                kind: SHUTDOWN,
                id: 3,
                payload: vec![],
            }
            .write(&mut raw.command)?;
            ensure!(
                receive(&mut raw)?.kind == OK,
                "Raw startup probe shutdown did not ACK"
            );
            ensure!(
                raw.wait(Duration::from_secs(10)),
                "Raw startup probe shutdown did not exit"
            );
            return Ok(());
        }
        if std::env::args().nth(1).as_deref() == Some("--desktop-probe") {
            let desktop = DesktopProbe::start()?;
            desktop.check(std::process::id(), "probe preflight")?;
            println!("PASS desktop probe preflight");
            return Ok(());
        }
        if std::env::args().nth(1).as_deref() == Some("--peer-child") {
            let path = std::env::args()
                .nth(2)
                .context("Missing peer browser path")?;
            let browser = Browser::launch(path)?;
            browser.create(
                "about:blank",
                Size {
                    width: 400,
                    height: 300,
                    dpr: 1.,
                },
                true,
            )?;
            println!("PEER_PID={}", browser.pid());
            std::io::stdout().flush()?;
            thread::sleep(Duration::from_millis(500));
            std::process::exit(0); // Intentionally skip all Rust destructors.
        }
        let path = std::env::args()
            .nth(1)
            .unwrap_or_else(|| DEFAULT_CHROME.into());
        let desktop = DesktopProbe::start()?;
        let browser = Browser::launch(&path)?;
        println!("HELLO_ACK {:?}", browser.hello);
        desktop.check(browser.pid(), "startup")?;
        ensure!(
            browser.list()?.is_empty(),
            "Browser must start with no targets"
        );
        let mut events = Vec::new();
        let viewport_url = "data:text/html,<script>function report(){document.title=innerWidth+'x'+innerHeight+'@'+devicePixelRatio}addEventListener('resize',report);setInterval(report,100);report()</script>";
        let n = browser.transcript().len();
        let first = browser.create(
            viewport_url,
            Size {
                width: 640,
                height: 480,
                dpr: 1.,
            },
            false,
        )?;
        ordered(&browser, n, CREATED, TARGET_RESULT)?;
        ensure!(first.active, "First target must activate");
        let n = browser.transcript().len();
        let second = browser.create(
            viewport_url,
            Size {
                width: 960,
                height: 540,
                dpr: 1.,
            },
            false,
        )?;
        ordered(&browser, n, CREATED, TARGET_RESULT)?;
        ensure!(!second.active, "Inactive target unexpectedly activated");
        collect_until(
            &browser,
            &mut events,
            |e| matches!(e,Event::Changed(t) if t.id==first.id&&t.title=="640x480@1"&&t.load==2),
        )?;
        collect_until(
            &browser,
            &mut events,
            |e| matches!(e,Event::Changed(t) if t.id==second.id&&t.title=="960x540@1"&&t.load==2),
        )?;
        println!(
            "PASS actual CSS viewport dimensions640x480@1 and960x540@1 reported by page JavaScript"
        );
        let list = browser.list()?;
        ensure!(
            list.len() == 2
                && list
                    .iter()
                    .any(|t| t.id == first.id && t.size == first.size)
                && list
                    .iter()
                    .any(|t| t.id == second.id && t.size == second.size),
            "Distinct-size target list mismatch: {list:?}"
        );
        desktop.check(browser.pid(), "two targets")?;
        let n = browser.transcript().len();
        let resized = browser.resize(
            second.id,
            Size {
                width: 800,
                height: 600,
                dpr: 1.,
            },
        )?;
        ordered(&browser, n, CHANGED, TARGET_RESULT)?;
        ensure!(
            resized.generation > second.generation && resized.size.width == 800,
            "Resize did not advance generation"
        );
        collect_until(
            &browser,
            &mut events,
            |e| matches!(e,Event::Changed(t) if t.id==second.id&&t.title=="800x600@1"&&t.generation>=resized.generation),
        )?;
        println!("PASS resized actual CSS viewport800x600@1");
        let dpr_two = browser.resize(
            first.id,
            Size {
                width: 640,
                height: 480,
                dpr: 2.,
            },
        )?;
        collect_until(
            &browser,
            &mut events,
            |e| matches!(e,Event::Changed(t) if t.id==first.id&&t.title=="640x480@2"&&t.generation>=dpr_two.generation),
        )?;
        browser.resize(
            first.id,
            Size {
                width: 640,
                height: 480,
                dpr: 1.,
            },
        )?;
        println!("PASS per-target actual CSS viewport640x480 with devicePixelRatio2");
        let n = browser.transcript().len();
        let activated = browser.activate(second.id)?;
        ordered(&browser, n, CHANGED, TARGET_RESULT)?;
        ensure!(
            activated.active && activated.generation > resized.generation,
            "Activation did not advance generation"
        );
        desktop.check(browser.pid(), "resize and activation")?;
        events.extend(browser.drain_events());
        let url = "data:text/html,<title>PRTS-M1-Navigation</title><h1>PRTS M1</h1>";
        let n = browser.transcript().len();
        browser.navigate(first.id, url)?;
        ordered(&browser, n, CHANGED, OK)?;
        collect_until(
            &browser,
            &mut events,
            |e| matches!(e,Event::Changed(t) if t.id==first.id&&t.title=="PRTS-M1-Navigation"&&t.load==2),
        )?;
        let n = browser.transcript().len();
        browser.close(second.id)?;
        ordered(&browser, n, CLOSED, OK)?;
        let list = browser.list()?;
        ensure!(
            list.len() == 1 && list[0].id == first.id && list[0].active,
            "Active close did not select surviving target"
        );
        events.extend(browser.drain_events());
        let n = browser.transcript().len();
        browser.close(first.id)?;
        ordered(&browser, n, CLOSED, OK)?;
        ensure!(browser.list()?.is_empty(), "Last target remains");
        browser.ping(0x12345678)?;
        thread::sleep(Duration::from_secs(6));
        browser.ping(0x87654321)?;
        println!(
            "PASS last-window closure keeps instance alive; idle heartbeat remains responsive"
        );
        let recreated = browser.create(
            "about:blank",
            Size {
                width: 480,
                height: 320,
                dpr: 1.,
            },
            false,
        )?;
        ensure!(
            recreated.id > second.id && recreated.active,
            "Recreated last-window target ID/activation invalid"
        );
        browser.close(recreated.id)?;
        ensure!(browser.list()?.is_empty(), "Recreated target did not close");
        println!("PASS fresh target recreation after last-window closure (profile keep-alive)");
        events.extend(browser.drain_events());
        for event in &events {
            println!("EVENT {event:?}");
        }
        desktop.check(browser.pid(), "all targets closed")?;
        browser.shutdown()?;
        println!(
            "PASS HELLO, distinct sizes, create/list/navigate/load/title/resize/activate/close ordering, SHUTDOWN->OK"
        );
        failure_cases(Path::new(&path))?;
        popup_case(Path::new(&path), &desktop)?;
        if std::env::var("PRTS_M1_SKIP_EXTENDED").as_deref() != Ok("1") {
            malformed_cases(Path::new(&path))?;
        } else {
            println!(
                "SKIP extended malformed/bootstrap/stalled-reader qualification (PRTS_M1_SKIP_EXTENDED=1)"
            );
        }
        println!(
            "PASS M1 control and visibility/cloaking/taskbar acceptance; foreground status as reported above; HWND creation is expected"
        );
        Ok(())
    }
}
