//! Reject a real GPU registration on the wire; verify sticky CPU recovery.
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use anyhow::{Context, ensure};
    use prtsd::{
        browser::{Browser, DEFAULT_CHROME, RawBrowser},
        browser_wire::{self as wire, Decoder, Record, Size},
    };
    use std::{
        os::windows::io::{FromRawHandle, OwnedHandle},
        path::Path,
        sync::mpsc,
        thread,
        time::Duration,
    };
    use windows::Win32::{
        Foundation::HANDLE,
        System::Threading::{OpenProcess, PROCESS_DUP_HANDLE},
    };
    let path = std::env::var("PRTS_CHROME_PATH").unwrap_or_else(|_| DEFAULT_CHROME.into());
    // Software rendering must not negotiate a texture fast path.
    let browser = Browser::connect(RawBrowser::launch_with_switches(
        Path::new(&path),
        &["--disable-gpu"],
    )?)?;
    ensure!(
        browser.hello.frame_modes == 2,
        "Disabled GPU advertised textures"
    );
    browser.shutdown()?;
    drop(browser);
    // A valid offer naming a nonexistent hardware adapter must intersect to CPU.
    let luid = prtsd::d3d::shared()?.lock().unwrap().luid;
    let mut mismatch = RawBrowser::launch(Path::new(&path))?;
    mismatch.wait_startup()?;
    let mut p = Vec::new();
    for n in [1u16, 0, 1, 0] {
        p.extend(n.to_le_bytes());
    }
    p.extend(std::process::id().to_le_bytes());
    p.extend((luid ^ 1).to_le_bytes());
    p.extend(3u32.to_le_bytes());
    p.extend(2u32.to_le_bytes());
    Record {
        kind: 1,
        id: 1,
        payload: p,
    }
    .write(&mut mismatch.command)?;
    let ack = Record::read(&mut mismatch.event)?;
    ensure!(
        ack.kind == 2 && ack.payload[16..20] == 2u32.to_le_bytes(),
        "Mismatched LUID advertised texture mode"
    );
    Record {
        kind: 6,
        id: 3,
        payload: vec![],
    }
    .write(&mut mismatch.command)?;
    ensure!(
        Record::read(&mut mismatch.event)?.kind == 7 && mismatch.wait(Duration::from_secs(5)),
        "Mismatched-adapter shutdown failed"
    );
    drop(mismatch);
    println!("Mismatched adapter LUID negotiated CPU only");
    let mut raw = RawBrowser::launch(Path::new(&path))?;
    raw.wait_startup()?;
    let process = unsafe { OpenProcess(PROCESS_DUP_HANDLE, false, raw.pid)? };
    let _process = unsafe { OwnedHandle::from_raw_handle(process.0) };
    let (tx, rx) = mpsc::channel();
    let mut workers = Vec::new();
    for mut file in [raw.event.try_clone()?, raw.frame.try_clone()?] {
        let tx = tx.clone();
        workers.push(thread::spawn(move || {
            while let Ok(record) = Record::read(&mut file) {
                if tx.send(record).is_err() {
                    break;
                }
            }
        }));
    }
    let receive = || {
        rx.recv_timeout(Duration::from_secs(5))
            .context("Recovery wire response timeout")
    };
    let mut hello = Vec::new();
    for n in [1u16, 0, 1, 0] {
        hello.extend(n.to_le_bytes());
    }
    hello.extend(std::process::id().to_le_bytes());
    hello.extend(prtsd::d3d::shared()?.lock().unwrap().luid.to_le_bytes());
    hello.extend(3u32.to_le_bytes());
    hello.extend(2u32.to_le_bytes());
    Record {
        kind: 1,
        id: 1,
        payload: hello,
    }
    .write(&mut raw.command)?;
    let ack = receive()?;
    ensure!(
        ack.kind == 2 && ack.payload[16..20] == 3u32.to_le_bytes(),
        "GPU recovery test needs texture negotiation"
    );
    let mut create = Vec::new();
    wire::string(
        "data:text/html,<body style='background:rgb(220,30,60)'>",
        &mut create,
    )?;
    Size {
        width: 320,
        height: 240,
        dpr: 1.,
    }
    .encode(&mut create)?;
    create.push(1);
    Record {
        kind: 0x100,
        id: 3,
        payload: create,
    }
    .write(&mut raw.command)?;
    let target = loop {
        let r = receive()?;
        if r.kind == 0x180 && r.id == 3 {
            break Decoder::new(&r.payload).target()?;
        }
    };
    let mut config = target.id.to_le_bytes().to_vec();
    config.push(1);
    config.extend(2u32.to_le_bytes());
    config.extend(30u32.to_le_bytes());
    Record {
        kind: 0x321,
        id: 5,
        payload: config,
    }
    .write(&mut raw.command)?;
    let mut pool = prtsd::browser_frames::Resources::default();
    let mut rejected = 0;
    let cpu_generation;
    loop {
        let r = receive()?;
        if r.kind == 0x300 && r.payload[24] == 1 {
            let mut p = 9u32.to_le_bytes().to_vec();
            wire::string("Acceptance deliberately rejects GPU import", &mut p)?;
            p.push(0);
            Record {
                kind: 3,
                id: r.id,
                payload: p,
            }
            .write(&mut raw.command)?;
            rejected += 1;
        } else if matches!(r.kind, 0x300 | 0x301 | 0x311) {
            pool.process(&r, HANDLE(process.0))?
                .write(&mut raw.command)?;
            if let Some(f) = pool.take_frame() {
                ensure!(
                    f.texture.is_none() && f.generation > target.generation + 1,
                    "Recovery did not invalidate GPU generation"
                );
                let center = (f.width as usize * 120 + 160) * 4;
                ensure!(
                    f.pixels[center..center + 4] == [60, 30, 220, 255],
                    "CPU fallback color mismatch"
                );
                cpu_generation = f.generation;
                break;
            }
        } else {
            ensure!(
                r.kind != 0x310,
                "Rejected resource published a texture frame"
            );
        }
    }
    ensure!(rejected > 0, "No GPU registration rejected");
    Record {
        kind: 6,
        id: 7,
        payload: vec![],
    }
    .write(&mut raw.command)?;
    loop {
        let r = receive()?;
        if r.kind == 7 && r.id == 7 {
            break;
        }
    }
    ensure!(raw.wait(Duration::from_secs(5)), "Recovery shutdown failed");
    for worker in workers {
        let _ = worker.join();
    }
    println!(
        "GPU import rejection -> exact CPU pixels, generation={cpu_generation}, rejected={rejected}; disabled-GPU negotiation and clean shutdown passed"
    );
    Ok(())
}
#[cfg(not(windows))]
fn main() {
    eprintln!("Windows required");
}
