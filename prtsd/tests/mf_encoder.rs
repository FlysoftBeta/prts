#![cfg(windows)]
use prtsd::{encoder::VideoEncoder, mf_encoder::MfEncoder};

/// Opt in on machines with an Intel/NVIDIA hardware encoder. Ordinary CI still
/// compiles the complete native backend without requiring a GPU or desktop.
#[test]
#[ignore = "requires a hardware H264 MFT; run --ignored --nocapture"]
fn hardware_encode_decode_dynamic_bitrate() -> anyhow::Result<()> {
    let (w, h) = (1280, 720);
    let mut encoder = MfEncoder::new(w, h, 2_000_000, 30)?;
    eprintln!("hardware encoder: {}", encoder.name());
    let mut decoder = openh264::decoder::Decoder::new()?;
    let start = std::time::Instant::now();
    let mut frames = 0;
    let mut bytes = 0;
    let mut capped_bytes = 0;
    let mut encode_time = std::time::Duration::ZERO;
    let mut forced_idr = false;
    let mut baseline = false;
    let mut idrs = 0;
    for frame in 0..90 {
        if frame == 45 {
            encoder.set_bitrate(150_000)?;
        }
        let mut bgra = vec![0u8; (w * h * 4) as usize];
        for pixel in bgra.as_chunks_mut::<4>().0 {
            pixel.copy_from_slice(&[30, 60, 100, 255]);
        }
        let x = frame as usize * 8 % (w as usize - 64);
        for y in 80..144 {
            for dx in x..x + 64 {
                bgra[(y * w as usize + dx) * 4..][..4].copy_from_slice(&[255, 255, 255, 255]);
            }
        }
        let before = std::time::Instant::now();
        let encoded = encoder.encode(&bgra, w as usize, h as usize, frame == 0 || frame == 25)?;
        encode_time += before.elapsed();
        bytes += encoded.len();
        if frame >= 45 {
            capped_bytes += encoded.len();
        }
        for nal in openh264::nal_units(&encoded) {
            let prefix = if nal.starts_with(&[0, 0, 0, 1]) { 4 } else { 3 };
            let payload = &nal[prefix..];
            if payload[0] & 31 == 5 {
                idrs += 1;
            }
            if payload[0] & 31 == 7 {
                assert_eq!(payload[1], 66);
                baseline = true;
            }
            if (25..=27).contains(&frame) && payload[0] & 31 == 5 {
                forced_idr = true;
            }
            if let Some(picture) = decoder.decode(nal)? {
                use openh264::formats::YUVSource;
                assert_eq!(picture.dimensions(), (w as usize, h as usize));
                frames += 1;
            }
        }
    }
    eprintln!(
        "decoded={frames}/90 bytes={bytes} capped_bytes={capped_bytes} idrs={idrs} average_encode_ms={:.2} wall_ms={:.2}",
        encode_time.as_secs_f64() * 1000. / 90.,
        start.elapsed().as_secs_f64() * 1000.
    );
    assert!(frames >= 85, "hardware output stalled");
    assert!(
        baseline && forced_idr,
        "baseline SPS and requested IDR required"
    );
    assert!(
        idrs <= 5,
        "force-keyframe must not remain latched: {idrs} IDRs"
    );
    Ok(())
}

#[test]
#[ignore = "requires a hardware H264 MFT; run --ignored --nocapture"]
fn hardware_live_bitrate_cap_is_effective() -> anyhow::Result<()> {
    use prtsd::{
        model::{Size, Target},
        video::render,
    };
    let target = Target::new(1, "about:blank".into(), Size::default(), true);
    let (w, h) = target.size.device();
    let (mut encoder, warning) = prtsd::encoder::Selected::new("qsv", w, h, 4_000_000, 30)?;
    assert!(warning.is_none(), "hardware must initialize");
    let mut decoder = openh264::decoder::Decoder::new()?;
    let (mut baseline_bytes, mut capped_bytes, mut capped_frames) = (0usize, 0usize, 0);
    let mut hardware_capped_bytes = 0;
    let mut fell_back = false;
    let mut encode_time = std::time::Duration::ZERO;
    for frame in 0..900 {
        if frame == 300 {
            encoder.set_bitrate(1_000_000)?;
        }
        if frame == 600 {
            encoder.set_bitrate(150_000)?;
        }
        let bgra = render(&target, frame, frame as f64 / 30.);
        let start = std::time::Instant::now();
        let bytes = match encoder.encode(&bgra, w as usize, h as usize, frame == 0) {
            Ok(bytes) => bytes,
            Err(e) if frame >= 600 && !fell_back => {
                eprintln!("measured hardware cap failure: {e:#}");
                encoder.fallback()?;
                fell_back = true;
                encoder.encode(&bgra, w as usize, h as usize, true)?
            }
            Err(e) => return Err(e),
        };
        encode_time += start.elapsed();
        if (150..300).contains(&frame) {
            baseline_bytes += bytes.len();
        }
        if (450..600).contains(&frame) {
            hardware_capped_bytes += bytes.len();
        }
        if frame >= 750 {
            capped_bytes += bytes.len();
        }
        for nal in openh264::nal_units(&bytes) {
            if decoder.decode(nal)?.is_some() && frame >= 750 {
                capped_frames += 1;
            }
        }
    }
    let baseline_bps = baseline_bytes * 8 / 5;
    let capped_bps = capped_bytes * 8 / 5;
    let hardware_capped_bps = hardware_capped_bytes * 8 / 5;
    eprintln!(
        "{} baseline_bps={baseline_bps} native_1M_cap_bps={hardware_capped_bps} capped_bps={capped_bps} capped_decoded={capped_frames}/150 encode_ms_avg={:.2}",
        encoder.name(),
        encode_time.as_secs_f64() * 1000. / 900.
    );
    assert!(baseline_bps > 500_000, "baseline source must exercise RC");
    assert!(
        hardware_capped_bps <= 1_250_000 && hardware_capped_bps < baseline_bps / 2,
        "native dynamic bitrate must take effect"
    );
    assert!(
        capped_bps <= 200_000 && capped_bps < baseline_bps / 2,
        "live bitrate cap was ignored"
    );
    assert!(capped_frames >= 125, "cap must retain useful frame rate");
    Ok(())
}

/// Strict resource-lifetime gate for real hardware activation and teardown.
#[test]
#[ignore = "requires Intel QSV; run --ignored --nocapture"]
fn hardware_encoder_creation_does_not_leak_handles() -> anyhow::Result<()> {
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};
    let mut counts = Vec::new();
    for _ in 0..20 {
        drop(MfEncoder::new(640, 480, 2_000_000, 30)?);
        let mut minimum = u32::MAX;
        for _ in 0..20 {
            let mut count = 0;
            unsafe {
                GetProcessHandleCount(GetCurrentProcess(), &mut count)?;
            }
            minimum = minimum.min(count);
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        counts.push(minimum);
    }
    eprintln!("QSV create/close settled process handles: {counts:?}");
    anyhow::ensure!(
        counts.iter().all(|count| *count <= counts[0] + 2),
        "Hardware MFT handle growth: {counts:?}"
    );
    Ok(())
}
