#![cfg(windows)]
use openh264::formats::YUVSource;
use prtsd::{
    d3d,
    encoder::VideoEncoder,
    model::{Size, Target},
    video::render,
    vpl_encoder::VplEncoder,
};
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;

#[test]
#[ignore = "requires Intel oneVPL hardware; run --release --ignored --nocapture"]
fn direct_hardware_low_latency_and_live_caps() -> anyhow::Result<()> {
    let target = Target::new(1, "about:blank".into(), Size::default(), true);
    let (w, h) = target.size.device();
    for gpu_input in [false, true] {
        let gpu = d3d::shared()?;
        let texture = gpu
            .lock()
            .unwrap()
            .texture(w, h, DXGI_FORMAT_B8G8R8A8_UNORM, false)?;
        let frame = d3d::Frame { texture, gpu };
        let mut encoder = VplEncoder::new(w, h, 4_000_000, 30)?;
        let mut decoder = openh264::decoder::Decoder::new()?;
        let mut times = Vec::new();
        let mut bytes = [0usize; 3];
        let mut idrs = Vec::new();
        for index in 0..900 {
            if index == 300 {
                encoder.set_bitrate(1_000_000)?;
            }
            if index == 600 {
                encoder.set_bitrate(150_000)?;
            }
            let mut pixels = render(&target, index, index as f64 / 30.);
            // Capped VBR does not insert CBR filler into this mostly static UI.
            // Low-amplitude temporal detail exercises the high-rate controller
            // while remaining quantizable at low rates; keep the same stimulus
            // for all three rates and preserve every cap/latency assertion.
            let mut random = index as u32 ^ 0xa531_72c9;
            for pixel in pixels.as_chunks_mut::<4>().0 {
                random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                let delta = (random >> 27) as i16 - 16;
                for channel in &mut pixel[..3] {
                    *channel = (i16::from(*channel) + delta).clamp(0, 255) as u8;
                }
            }
            if gpu_input {
                let g = frame.gpu.lock().unwrap();
                unsafe {
                    g.context.UpdateSubresource(
                        &frame.texture,
                        0,
                        None,
                        pixels.as_ptr().cast(),
                        w * 4,
                        0,
                    );
                }
            }
            let start = Instant::now();
            let encoded = if gpu_input {
                encoder.encode_texture(&frame, index == 0 || index == 25)?
            } else {
                encoder.encode(&pixels, w as usize, h as usize, index == 0 || index == 25)?
            };
            times.push(start.elapsed().as_secs_f64() * 1000.);
            if index % 300 >= 150 {
                bytes[index as usize / 300] += encoded.len();
            }
            let mut decoded = 0;
            let mut idr = false;
            for nal in openh264::nal_units(&encoded) {
                let prefix = if nal.starts_with(&[0, 0, 0, 1]) { 4 } else { 3 };
                idr |= nal[prefix] & 31 == 5;
                if nal[prefix] & 31 == 7 {
                    assert_eq!(nal[prefix + 1], 66, "baseline SPS required");
                }
                if let Some(picture) = decoder.decode(nal)? {
                    assert_eq!(picture.dimensions(), (w as usize, h as usize));
                    decoded += 1;
                }
            }
            assert_eq!(
                decoded, 1,
                "every hardware request must decode immediately at frame {index}"
            );
            if idr {
                idrs.push(index);
            }
        }
        let rates = bytes.map(|b| b * 8 / 5);
        times.sort_by(f64::total_cmp);
        let p95 = times[times.len() * 95 / 100];
        println!(
            "oneVPL gpu={gpu_input} baseline_bps={} cap1M_bps={} cap150k_bps={} encode_ms_avg={:.2} p95_ms={p95:.2} decoded=900/900",
            rates[0],
            rates[1],
            rates[2],
            times.iter().sum::<f64>() / times.len() as f64
        );
        assert!(rates[0] > 500_000, "fixture must exercise RC");
        assert!(
            rates[1] <= 1_250_000 && rates[1] < rates[0] / 2,
            "native dynamic 1M cap ignored"
        );
        assert!(
            rates[2] <= 200_000 && rates[2] < rates[0] / 2,
            "native 150k cap ignored; no software fallback allowed"
        );
        assert!(
            p95 <= 100.,
            "oneVPL encode p95 exceeds existing hardware latency budget"
        );
        for required in [0, 25, 300, 600] {
            assert!(
                idrs.contains(&required),
                "initial/forced/reset IDR missing {required}: {idrs:?}"
            );
        }
        assert!(
            idrs.len() <= 25,
            "forced keyframe remained latched: {idrs:?}"
        );
    }
    Ok(())
}

#[test]
#[ignore = "requires Intel oneVPL and MF hardware; run --release --ignored --nocapture"]
fn selected_hardware_recovery_preserves_working_codecs() -> anyhow::Result<()> {
    use prtsd::encoder::Selected;
    let pixels = [20, 180, 90, 255].repeat(640 * 480);
    let (mut encoder, warning) = Selected::new("qsv", 640, 480, 2_000_000, 30)?;
    assert!(warning.is_none());
    for expected in ["oneVPL", "Quick Sync", "software/openh264"] {
        assert!(
            encoder.name().contains(expected),
            "wrong recovery codec: {}",
            encoder.name()
        );
        let data = encoder.encode(&pixels, 640, 480, true)?;
        let mut decoder = openh264::decoder::Decoder::new()?;
        let mut pictures = 0;
        for nal in openh264::nal_units(&data) {
            if decoder.decode(nal)?.is_some() {
                pictures += 1;
            }
        }
        assert_eq!(pictures, 1, "recovered codec must produce a decoded frame");
        if expected != "software/openh264" {
            encoder.fallback()?;
        }
        if expected == "oneVPL" {
            assert_eq!(encoder.preference(), "mf");
        }
    }
    let (mut strict, warning) = Selected::new("onevpl", 640, 480, 2_000_000, 30)?;
    assert!(warning.is_none());
    assert!(
        strict.fallback().is_err(),
        "forced oneVPL must never silently change codec"
    );
    assert!(strict.name().contains("oneVPL"));
    assert!(!strict.encode(&pixels, 640, 480, true)?.is_empty());
    Ok(())
}

#[test]
#[ignore = "requires Intel oneVPL hardware; run --release --ignored --nocapture"]
fn rolling_refresh_preserves_immediate_decode_and_forced_idr() -> anyhow::Result<()> {
    let mut encoder = VplEncoder::new_with_intra_refresh(640, 480, 1_000_000, 30, true)?;
    let target = Target::new(
        1,
        "about:blank".into(),
        Size {
            width: 640,
            height: 480,
            dpr: 1.,
        },
        true,
    );
    let mut decoder = openh264::decoder::Decoder::new()?;
    let mut idrs = Vec::new();
    for index in 0..130 {
        if index == 60 {
            encoder.set_bitrate(500_000)?;
        }
        let pixels = render(&target, index, index as f64 / 30.);
        let bytes = encoder.encode(&pixels, 640, 480, index == 45)?;
        let mut pictures = 0;
        for nal in openh264::nal_units(&bytes) {
            let prefix = if nal.starts_with(&[0, 0, 0, 1]) { 4 } else { 3 };
            if nal[prefix] & 31 == 5 {
                idrs.push(index);
            }
            if decoder.decode(nal)?.is_some() {
                pictures += 1;
            }
        }
        assert_eq!(pictures, 1, "intra refresh buffered a frame at {index}");
    }
    assert_eq!(
        idrs,
        vec![0, 45, 60],
        "rolling refresh must preserve explicit/reset IDR without periodic full-frame spikes"
    );
    println!("oneVPL rolling refresh130/130 immediate pictures, forced/reset IDRs={idrs:?}");
    Ok(())
}

#[test]
#[ignore = "requires Intel oneVPL hardware; run --release --ignored --nocapture"]
fn direct_hardware_creation_does_not_leak_handles() -> anyhow::Result<()> {
    use windows::Win32::System::Threading::{GetCurrentProcess, GetProcessHandleCount};
    let mut counts = Vec::new();
    for _ in 0..20 {
        drop(VplEncoder::new(640, 480, 2_000_000, 30)?);
        let mut minimum = u32::MAX;
        for _ in 0..20 {
            let mut count = 0;
            unsafe {
                GetProcessHandleCount(GetCurrentProcess(), &mut count)?;
            }
            minimum = minimum.min(count);
            std::thread::sleep(Duration::from_millis(100));
        }
        counts.push(minimum);
    }
    println!("oneVPL settled handles: {counts:?}");
    assert!(
        counts.iter().all(|count| *count <= counts[0] + 2),
        "oneVPL handle growth: {counts:?}"
    );
    Ok(())
}
