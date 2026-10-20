//! Real Viz/shared-section CPU fallback acceptance.
#[cfg(windows)]
#[path = "support/desktop.rs"]
mod desktop;
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    acceptance::run()
}
#[cfg(not(windows))]
fn main() {
    eprintln!("browser_m3 requires Windows");
}
#[cfg(windows)]
mod acceptance {
    //! Real Viz/shared-section CPU fallback acceptance (no WebRTC dependency).
    use anyhow::{Result, ensure};
    use prtsd::{
        browser::{Browser, DEFAULT_CHROME},
        browser_wire::Size,
    };
    use std::time::{Duration, Instant};

    fn frame(
        browser: &Browser,
        target: u64,
        generation: u64,
        width: u32,
        height: u32,
    ) -> Result<prtsd::browser_frames::CpuFrame> {
        let end = Instant::now() + Duration::from_secs(15);
        while Instant::now() < end {
            if let Some(f) = browser.poll_frame()?
                && (f.target, f.generation, f.width, f.height)
                    == (target, generation, width, height)
            {
                return Ok(f);
            }
            browser.drain_events();
            std::thread::sleep(Duration::from_millis(10));
        }
        anyhow::bail!(
            "No matching captured frame: target={target} generation={generation} {width}x{height}"
        )
    }
    fn color(f: &prtsd::browser_frames::CpuFrame, expected: [u8; 4]) -> Result<()> {
        let i = (f.width as usize * (f.height as usize / 2) + f.width as usize / 2) * 4;
        ensure!(
            f.pixels[i..i + 4]
                .iter()
                .zip(expected)
                .all(|(a, b)| a.abs_diff(b) <= 2),
            "Incorrect BGRA color {:?}, expected {:?}",
            &f.pixels[i..i + 4],
            expected
        );
        Ok(())
    }
    pub fn run() -> Result<()> {
        let desktop = super::desktop::DesktopProbe::start()?;
        let browser = Browser::launch(
            std::env::var("PRTS_CHROME_PATH").unwrap_or_else(|_| DEFAULT_CHROME.into()),
        )?;
        let a = browser.create(
            "data:text/html,<body style='margin:0;background:rgb(220,30,60)'>",
            Size {
                width: 640,
                height: 480,
                dpr: 1.,
            },
            true,
        )?;
        let a = browser.configure_frames(a.id, 30)?;
        let f = frame(&browser, a.id, a.generation, 640, 480)?;
        color(&f, [60, 30, 220, 255])?;
        ensure!(
            f.timestamp_us > 0 && f.timestamp_us < 5_000_000,
            "Timestamp is not relative to HELLO_ACK: {}",
            f.timestamp_us
        );
        println!(
            "CPU FALLBACK first frame: {}x{} stride={} BGRA verified pid={}",
            f.width,
            f.height,
            f.width * 4,
            browser.pid()
        );
        desktop.check(browser.pid(), "M3 first CPU frame")?;
        let b = browser.create(
            "data:text/html,<body style='margin:0;background:rgb(20,180,90)'>",
            Size {
                width: 800,
                height: 600,
                dpr: 1.,
            },
            false,
        )?;
        let b = browser.configure_frames(b.id, 30)?;
        // Both subscriptions share two credits; the inactive target also renders.
        ensure!(!b.active, "Background target unexpectedly active");
        let f = frame(&browser, b.id, b.generation, 800, 600)?;
        color(&f, [90, 180, 20, 255])?;
        desktop.check(browser.pid(), "M3 two subscribed targets")?;
        println!("Background target capture passed");
        let third = browser.create(
            "about:blank",
            Size {
                width: 320,
                height: 240,
                dpr: 1.,
            },
            false,
        )?;
        ensure!(
            browser.configure_frames(third.id, 30).is_err(),
            "Third subscription exceeded the shared pool"
        );
        browser.close(third.id)?;
        browser.refresh_frame(a.id)?;
        color(
            &frame(&browser, a.id, a.generation, 640, 480)?,
            [60, 30, 220, 255],
        )?;
        browser.configure_frames(a.id, 0)?;

        for _ in 0..3 {
            browser.refresh_frame(b.id)?;
            color(
                &frame(&browser, b.id, b.generation, 800, 600)?,
                [90, 180, 20, 255],
            )?;
        }
        println!("Static refresh passed");
        let b = browser.resize(
            b.id,
            Size {
                width: 480,
                height: 320,
                dpr: 2.,
            },
        )?;
        browser.refresh_frame(b.id)?;
        color(
            &frame(&browser, b.id, b.generation, 960, 640)?,
            [90, 180, 20, 255],
        )?;
        desktop.check(browser.pid(), "M3 resized CPU capture")?;
        println!("Resize/DPR2 passed");
        browser.navigate(b.id,"data:text/html,<body style='margin:0;background:rgb(10,40,230)'><script>setInterval(()=>document.body.style.backgroundColor=(Math.floor(Date.now()/33)&1)?'rgb(10,40,230)':'rgb(220,30,60)',33)</script>")?;
        let cpu_before = browser.resource_usage()?.0;
        let start = Instant::now();
        let mut frames = 0;
        while start.elapsed() < Duration::from_secs(3) {
            if browser.poll_frame()?.is_some() {
                frames += 1;
            }
            browser.drain_events();
            std::thread::sleep(Duration::from_millis(5));
        }
        ensure!(frames >= 10, "Animation capture stalled");
        println!(
            "Navigation/animation: {frames} frames / {:.2}s",
            start.elapsed().as_secs_f64()
        );
        let cpu = browser.resource_usage()?.0 - cpu_before;
        println!(
            "Browser tree animation CPU: {:.3}s / {:.3}s wall ({:.1}% of one core; includes renderer/GPU process CPU)",
            cpu,
            start.elapsed().as_secs_f64(),
            cpu / start.elapsed().as_secs_f64() * 100.
        );
        let mut memory = Vec::new();
        for n in 0..4 {
            browser.ping(n)?;
            browser.poll_frame()?;
            let before = browser.frame_counts().0;
            std::thread::sleep(Duration::from_secs(2));
            browser.poll_frame()?;
            let count = browser.frame_counts().0 - before;
            ensure!(count <= 2, "Slow consumer exceeded credits: {count}");
            println!("Slow-consumer cycle {n}: {count} queued descriptors (limit 2)");
            let (_, peak, sections) = browser.resource_usage()?;
            ensure!(sections <= 2, "Imported pool exceeds two sections");
            memory.push(peak);
            println!(
                "Slow consumer memory: browser tree peak commit={} KiB, imported sections={sections}",
                peak / 1024
            );
        }
        ensure!(
            memory[3] <= memory[0] + 32 * 1024 * 1024,
            "Slow consumer memory grew by more than 32 MiB: {memory:?}"
        );
        let b = browser.configure_frames_with_limit(b.id, 30, 1)?;
        frame(&browser, b.id, b.generation, 960, 640)?;
        browser.ping(0xabc)?;
        browser.poll_frame()?;
        let before = browser.frame_counts().0;
        std::thread::sleep(Duration::from_secs(2));
        browser.poll_frame()?;
        ensure!(
            browser.frame_counts().0 - before <= 1,
            "Requested one-credit limit was exceeded"
        );
        println!("Requested max_inflight=1 honored");
        let b = browser.configure_frames_with_limit(b.id, 10, 1)?;
        frame(&browser, b.id, b.generation, 960, 640)?;
        let start = Instant::now();
        let mut paced = 0;
        while start.elapsed() < Duration::from_secs(2) {
            if browser.poll_frame()?.is_some() {
                paced += 1;
            }
            browser.drain_events();
            std::thread::sleep(Duration::from_millis(5));
        }
        ensure!(
            (3..=22).contains(&paced),
            "10 FPS pacing failed: {paced} frames / 2 seconds"
        );
        println!(
            "10 FPS ceiling: {paced} frames / {:.2}s",
            start.elapsed().as_secs_f64()
        );
        browser.close(b.id)?;
        browser.configure_frames(a.id, 30)?;
        let a = browser.list()?.into_iter().find(|t| t.id == a.id).unwrap();
        browser.refresh_frame(a.id)?;
        color(
            &frame(&browser, a.id, a.generation, 640, 480)?,
            [60, 30, 220, 255],
        )?;
        browser.configure_frames(a.id, 0)?;
        for _ in 0..10 {
            browser.poll_frame()?;
            browser.drain_events();
            std::thread::sleep(Duration::from_millis(30));
        }
        let before = browser.frame_counts().0;
        std::thread::sleep(Duration::from_millis(500));
        browser.poll_frame()?;
        ensure!(
            browser.frame_counts().0 == before,
            "Capture continued with no subscribers"
        );
        desktop.check(browser.pid(), "M3 no subscribers")?;
        println!("No-subscriber capture pause passed");
        browser.close(a.id)?;
        browser.poll_frame()?;
        browser.shutdown()?;
        println!("M3 CPU frame acceptance passed");
        Ok(())
    }
}
