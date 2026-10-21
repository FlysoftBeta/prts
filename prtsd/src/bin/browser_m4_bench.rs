//! Fixed-rate capture + MF/oneVPL measurements; PRTS_TEST_ENCODER=mf|onevpl.
#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use anyhow::ensure;
    use prtsd::{
        browser::{Browser, DEFAULT_CHROME},
        browser_wire::Size,
        encoder::VideoEncoder,
    };
    use std::time::{Duration, Instant};
    use windows::Win32::{
        Foundation::FILETIME,
        System::Threading::{GetCurrentProcess, GetProcessTimes},
    };
    fn cpu() -> anyhow::Result<f64> {
        let (mut c, mut e, mut k, mut u) = (
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
            FILETIME::default(),
        );
        unsafe {
            GetProcessTimes(GetCurrentProcess(), &mut c, &mut e, &mut k, &mut u)?;
        }
        Ok((((k.dwHighDateTime as u64) << 32 | k.dwLowDateTime as u64)
            + ((u.dwHighDateTime as u64) << 32 | u.dwLowDateTime as u64)) as f64
            / 10_000_000.)
    }
    let browser = Browser::launch(
        std::env::var("PRTS_CHROME_PATH").unwrap_or_else(|_| DEFAULT_CHROME.into()),
    )?;
    let target = browser.create("data:text/html,<style>body{margin:0;background:rgb(20,80,140)}div{position:absolute;width:200px;height:200px;background:white;animation:m 2s linear infinite alternate}@keyframes m{to{transform:translate(600px,300px);background:red}}</style><div></div>", Size { width:960, height:640, dpr:1. }, true)?;
    for (w, h) in [(960, 640), (1920, 1080)] {
        let target = browser.resize(
            target.id,
            Size {
                width: w,
                height: h,
                dpr: 1.,
            },
        )?;
        let target = browser.configure_frames(target.id, 30)?;
        let preference = std::env::var("PRTS_TEST_ENCODER").unwrap_or_else(|_| "mf".into());
        ensure!(
            matches!(preference.as_str(), "mf" | "onevpl"),
            "benchmark requires explicit MF or oneVPL"
        );
        let (mut encoder, warning) =
            prtsd::encoder::Selected::new(&preference, w, h, 4_000_000, 30)?;
        ensure!(
            warning.is_none() && encoder.name() != "software/openh264",
            "hardware benchmark cannot fall back"
        );
        let mut count = 0;
        let mut encode = Duration::ZERO;
        let mut first = true;
        let mut byte_count = 0usize;
        let mut times = Vec::new();
        let mut backend = "";
        let mut input = "";
        let warm = Instant::now();
        let mut origin = Instant::now();
        let mut browser_cpu = 0.;
        let mut daemon_cpu = 0.;
        while origin.elapsed() < Duration::from_secs(8) {
            let Some(frame) = browser.poll_frame()? else {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            };
            if (frame.target, frame.generation) != (target.id, target.generation) {
                continue;
            }
            let start = Instant::now();
            let bytes = if let Some(texture) = &frame.texture {
                backend = "gpu-texture";
                input = "d3d11-zero-copy";
                encoder.encode_texture(texture, first)?
            } else {
                backend = "cpu-section";
                input = "cpu-nv12";
                encoder.encode(&frame.pixels, w as usize, h as usize, first)?
            };
            first = false;
            if warm.elapsed() < Duration::from_secs(2) {
                origin = Instant::now();
                browser_cpu = browser.resource_usage()?.0;
                daemon_cpu = cpu()?;
                count = 0;
                encode = Duration::ZERO;
                byte_count = 0;
                times.clear();
            } else if !bytes.is_empty() {
                count += 1;
                encode += start.elapsed();
                times.push(start.elapsed().as_secs_f64() * 1000.);
                byte_count += bytes.len();
            }
            browser.drain_events();
        }
        ensure!(count > 30, "Benchmark stalled");
        let wall = origin.elapsed().as_secs_f64();
        times.sort_by(f64::total_cmp);
        let p95 = times[times.len() * 95 / 100];
        println!(
            "{backend} {input} {w}x{h}: frames={count} wall={wall:.3}s browser_tree_cpu={:.2}% daemon_pipeline_cpu={:.2}% fps={:.2} encode_ms={:.3}",
            (browser.resource_usage()?.0 - browser_cpu) / wall * 100.,
            (cpu()? - daemon_cpu) / wall * 100.,
            count as f64 / wall,
            encode.as_secs_f64() * 1000. / count as f64
        );
        println!(
            "ENCODER_BENCH {}",
            serde_json::json!({"encoder":encoder.name(),"capture":backend,"input":input,"width":w,"height":h,"target_bps":4_000_000,"measured_bps":byte_count as f64*8./wall,"frames":count,"fps":count as f64/wall,"encode_ms_avg":encode.as_secs_f64()*1000./count as f64,"encode_ms_p95":p95,"browser_cpu_percent":(browser.resource_usage()?.0-browser_cpu)/wall*100.,"daemon_cpu_percent":(cpu()?-daemon_cpu)/wall*100.})
        );
        browser.configure_frames(target.id, 0)?;
    }
    browser.close(target.id)?;
    browser.shutdown()?;
    Ok(())
}
#[cfg(not(windows))]
fn main() {
    eprintln!("Windows required");
}
