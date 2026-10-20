//! I1 lifecycle, tokens and concurrent-instance acceptance. Media gates use M4/M5 runners.
use anyhow::{Result, ensure};
use prtsd::{
    browser::{Browser, DEFAULT_CHROME},
    browser_wire::Size,
    isolation::{Config, win},
};
fn main() -> Result<()> {
    if std::env::args().any(|a| a == "--install-capability") {
        let capability = prtsd::isolation::state::Capability::named("prtsInstallFiles")?;
        println!(
            "{}",
            prtsd::isolation::state::sid_string(capability.entry().Sid)?
        );
        return Ok(());
    }
    if std::env::args().any(|a| a == "--i1-native-probe") {
        use std::{io::Write, os::windows::io::FromRawHandle};
        let pipes = std::env::args()
            .find_map(|a| a.strip_prefix("--prts-pipe=").map(str::to_owned))
            .unwrap();
        let event = pipes
            .split(',')
            .find_map(|s| s.strip_prefix("evt:0x"))
            .unwrap();
        let event = usize::from_str_radix(event, 16)?;
        let mut event = unsafe { std::fs::File::from_raw_handle(event as _) };
        event.write_all(serde_json::to_string(&win::native_probe()?)?.as_bytes())?;
        return Ok(());
    }
    if std::env::args().any(|a| a == "--native-check" || a == "--network-check") {
        use std::io::Read;
        let path = std::path::Path::new(DEFAULT_CHROME).with_file_name("prts_i1_probe.exe");
        std::fs::copy(std::env::current_exe()?, &path)?;
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0))?;
        let connect = format!("--i1-connect=127.0.0.1:{}", listener.local_addr()?.port());
        let mut raw = prtsd::browser::RawBrowser::launch_with_switches(
            &path,
            &["--i1-native-probe", &connect],
        )?;
        ensure!(
            raw.wait(std::time::Duration::from_secs(30)),
            "native probe hung"
        );
        let mut report = String::new();
        raw.event.read_to_string(&mut report)?;
        eprintln!("I1 native_probe={report}");
        ensure!(raw.exit_code()? == 0, "native probe failed");
        std::fs::remove_file(path)?;
        return Ok(());
    }
    if std::env::args().any(|a| a == "--software-check") {
        let raw = prtsd::browser::RawBrowser::launch_with_switches(
            std::path::Path::new(DEFAULT_CHROME),
            &["--use-angle=d3d11-warp"],
        )?;
        let browser = Browser::connect(raw)?;
        let result = browser.require_hardware_gpu();
        eprintln!("I1 deliberate WARP GPU report={}", browser.gpu_info()?);
        ensure!(result.is_err(), "software rendering passed hardware gate");
        browser.shutdown()?;
        eprintln!("I1 PASS deliberate software rendering rejected");
        return Ok(());
    }
    let config = Config::from_env()?;
    eprintln!("I1 config={}", serde_json::to_string(&config)?);
    let a = Browser::launch(DEFAULT_CHROME)?;
    let b = Browser::launch(DEFAULT_CHROME)?;
    let size = Size {
        width: 960,
        height: 640,
        dpr: 1.0,
    };
    let ta = a.create("https://example.org/", size.clone(), true)?;
    let tb = b.create("about:blank", size, true)?;
    a.configure_frames(ta.id, 30)?;
    b.configure_frames(tb.id, 30)?;
    ensure!(a.state_path() != b.state_path(), "shared tenant state");
    let files = [
        b.state_path().join("profile").join("i1-secret"),
        b.state_path().join("temp").join("i1-secret"),
    ];
    for file in &files {
        std::fs::write(file, "tenant-b-secret")?;
    }
    let mut frames = [0, 0];
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while std::time::Instant::now() < deadline && frames.iter().any(|v| *v < 20) {
        a.refresh_frame(ta.id)?;
        b.refresh_frame(tb.id)?;
        for (i, browser) in [&a, &b].iter().enumerate() {
            if browser.poll_frame()?.is_some() {
                frames[i] += 1;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    ensure!(
        frames.iter().all(|v| *v >= 20),
        "frame transport did not progress: {frames:?}"
    );
    for (label, browser) in [("a", &a), ("b", &b)] {
        eprintln!("I1 {label} active_gpu={}", browser.require_hardware_gpu()?);
        eprintln!(
            "I1 {label} pid={} state={} stats={}",
            browser.pid(),
            browser.state_path().display(),
            browser.isolation_stats()?
        );
        for pid in browser.process_ids()? {
            eprintln!("I1 {label} token={}", win::token_evidence(pid)?);
        }
    }
    let mut targets = b.process_ids()?;
    targets.push(std::process::id()); // The browser must never acquire broker duplication authority.
    let probe = win::cross_tenant_probe(a.pid(), &files, &targets)?;
    eprintln!("I1 cross_tenant={probe}");
    if matches!(config.profile.as_str(), "appcontainer" | "lpac") {
        ensure!(
            probe["files"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v["allowed"] == false && v["error"] == 5),
            "cross-tenant files not denied"
        );
        ensure!(
            probe["processes"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v["allowed"] == false && v["error"] == -2147024891i64),
            "cross-tenant processes not denied"
        );
    } else {
        ensure!(
            probe["files"]
                .as_array()
                .unwrap()
                .iter()
                .all(|v| v["allowed"] == true),
            "same-user job probe unexpectedly denied"
        );
    }
    let paths = [a.state_path(), b.state_path()];
    a.shutdown()?;
    b.shutdown()?;
    drop((a, b));
    ensure!(paths.iter().all(|p| !p.exists()), "state not removed");
    eprintln!("I1 PASS concurrent frame progress, job membership, token evidence, normal cleanup");
    Ok(())
}
