use openh264::{decoder::Decoder, formats::YUVSource};
use prtsd::{
    encoder::{Software, VideoEncoder},
    model::{Size, Target},
    video::render,
};
use std::time::Instant;

#[test]
fn software_dynamic_bitrate_preserves_prediction() -> anyhow::Result<()> {
    let target = Target::new(
        1,
        "about:blank".into(),
        Size {
            width: 320,
            height: 240,
            dpr: 1.0,
        },
        true,
    );
    let mut encoder = Software::new(1_000_000, 30)?;
    let mut decoder = Decoder::new()?;
    let mut idrs = 0;
    let mut decoded = 0;
    for frame in 0..60 {
        if frame >= 30 {
            encoder.set_bitrate(150_000 + frame as u32 * 100, 30)?;
        }
        let bgra = render(&target, frame, frame as f64 / 30.0);
        let data = encoder.encode(&bgra, 320, 240, false)?;
        for nal in openh264::nal_units(&data) {
            let prefix = if nal.starts_with(&[0, 0, 0, 1]) { 4 } else { 3 };
            if nal[prefix] & 31 == 5 {
                idrs += 1;
            }
            if decoder.decode(nal)?.is_some() {
                decoded += 1;
            }
        }
    }
    assert!(decoded >= 55);
    assert_eq!(idrs, 1, "bitrate feedback must not recreate encoder/IDR");
    Ok(())
}

#[test]
fn software_preserves_motion_at_150kbps_without_resizing() -> anyhow::Result<()> {
    let target = Target::new(1, "about:blank".into(), Size::default(), true);
    for bps in [4_000_000, 300_000, 150_000, 50_000] {
        let mut encoder = Software::new(bps, 30)?;
        let mut decoder = Decoder::new()?;
        let (mut encoded, mut bytes, mut encode_ms) = (0, 0, 0.0);
        for frame in 0..180 {
            let bgra = render(&target, frame, frame as f64 / 30.0);
            let start = Instant::now();
            let data = encoder.encode(&bgra, 1280, 720, frame == 0)?;
            encode_ms += start.elapsed().as_secs_f64() * 1000.0;
            if !data.is_empty() {
                encoded += 1;
                bytes += data.len();
                if let Some(decoded) = decoder.decode(&data)? {
                    assert_eq!(decoded.dimensions(), (1280, 720));
                }
            }
        }
        let fps = encoded as f64 / 6.0;
        println!(
            "software target={bps} measured_bps={} fps={fps:.2} encode_ms_avg={:.3}",
            bytes * 8 / 6,
            encode_ms / 180.0
        );
        if bps >= 150_000 {
            assert!(fps >= 15.0, "{bps}: {fps} fps");
        }
    }
    Ok(())
}
