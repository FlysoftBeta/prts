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
#[path = "support/desktop.rs"]
mod desktop;

#[cfg(windows)]
mod acceptance {
    use super::desktop::DesktopProbe;
    use anyhow::{Context, Result, ensure};
    use prtsd::{
        browser::{Browser, DEFAULT_CHROME, RawBrowser, startup_timeout},
        browser_wire::*,
    };
    use std::{
        io::{BufRead, Write},
        path::Path,
        thread,
        time::{Duration, Instant},
    };
    use windows::Win32::{
        Foundation::{CloseHandle, WAIT_OBJECT_0},
        System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject},
    };

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
        // Both v1 and v2 are supported during the M5 migration.
        record.payload[..8].copy_from_slice(&[3, 0, 0, 0, 3, 0, 0, 0]);
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
                .or_else(|| std::env::var("PRTS_CHROME_PATH").ok())
                .unwrap_or_else(|| DEFAULT_CHROME.into());
            malformed_cases(Path::new(&path))?;
            println!("PASS extended bootstrap/framing/version/request-ID/backpressure acceptance");
            return Ok(());
        }
        if std::env::args().nth(1).as_deref() == Some("--raw-startup-probe") {
            let path = std::env::args()
                .nth(2)
                .or_else(|| std::env::var("PRTS_CHROME_PATH").ok())
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
            .or_else(|| std::env::var("PRTS_CHROME_PATH").ok())
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
