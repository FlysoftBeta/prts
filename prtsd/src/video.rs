use crate::{
    encoder::{Selected, VideoEncoder},
    model::{Options, Target},
};
use bytes::Bytes;
use font8x8::UnicodeFonts;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::watch;

#[derive(Clone)]
pub struct Frame {
    pub data: Bytes,
    pub capture: Instant,
    pub duration: Duration,
    pub epoch: u64,
    pub id: u64,
}
pub struct State {
    pub target: Option<Target>,
    pub viewers: usize,
    pub bitrate: u32,
    pub fps: u32,
    pub epoch: u64,
    pub keyframe: bool,
    pub stop: bool,
}
#[derive(Default)]
pub struct Metrics {
    pub captured: u64,
    pub encoded: u64,
    pub dropped: u64,
    pub bytes: u64,
    pub encode_ms: Vec<f64>,
    pub capture_encode_ms: Vec<f64>,
    pub send_ms: Vec<f64>,
    pub failure: Option<String>,
    pub warnings: Vec<String>,
}
pub struct Video {
    #[cfg(windows)]
    pub browser: crate::frame_source::BrowserSlot,
    pub state: Arc<Mutex<State>>,
    pub metrics: Arc<Mutex<Metrics>>,
    pub frames: watch::Receiver<Option<Frame>>,
    pub keyframe: Arc<AtomicBool>,
    pub encoder_name: Arc<Mutex<String>>,
    pub worker: Option<std::thread::JoinHandle<()>>,
}
impl Video {
    pub fn start(options: &Options) -> Self {
        let state = Arc::new(Mutex::new(State {
            target: None,
            viewers: 0,
            bitrate: options.bitrate_caps.max_bps,
            fps: options.fps,
            epoch: 0,
            keyframe: true,
            stop: false,
        }));
        let metrics = Arc::new(Mutex::new(Metrics::default()));
        let keyframe = Arc::new(AtomicBool::new(true));
        let (tx, frames) = watch::channel(None);
        let (s, m, k) = (state.clone(), metrics.clone(), keyframe.clone());
        let encoder_name = Arc::new(Mutex::new("initializing".to_string()));
        let name = encoder_name.clone();
        let preference = options.encoder.clone();
        let initial_size = options.size.device();
        let initial_bps = options.bitrate_caps.max_bps;
        let initial_fps = options.fps;
        #[cfg(windows)]
        let browser: crate::frame_source::BrowserSlot = Default::default();
        let mut source: Box<dyn crate::frame_source::FrameSource> = {
            #[cfg(windows)]
            if options.source == "chromium" {
                Box::new(crate::frame_source::BrowserSource {
                    browser: browser.clone(),
                    subscribed: None,
                })
            } else {
                Box::new(crate::frame_source::Synthetic {
                    origin: Instant::now(),
                    frame: 0,
                })
            }
            #[cfg(not(windows))]
            {
                Box::new(crate::frame_source::Synthetic {
                    origin: Instant::now(),
                    frame: 0,
                })
            }
        };
        let worker = std::thread::spawn(move || {
            let mut encoded_id = 0;
            let (mut encoder, warning) = match Selected::new(
                &preference,
                initial_size.0,
                initial_size.1,
                initial_bps,
                initial_fps,
            ) {
                Ok(result) => result,
                Err(e) => {
                    m.lock().unwrap().failure = Some(e.to_string());
                    return;
                }
            };
            if let Some(warning) = warning {
                m.lock().unwrap().warnings.push(warning);
            }
            *name.lock().unwrap() = encoder.name().to_string();
            let mut hardware_disabled = encoder.name() == "software/openh264";
            let mut config = (initial_size.0, initial_size.1, initial_bps, initial_fps);
            let mut next = Instant::now();
            loop {
                let (target, bps, fps, epoch, key) = {
                    let mut s = s.lock().unwrap();
                    if s.stop {
                        break;
                    }
                    (
                        if s.viewers > 0 {
                            s.target.clone()
                        } else {
                            None
                        },
                        s.bitrate,
                        s.fps,
                        s.epoch,
                        std::mem::take(&mut s.keyframe),
                    )
                };
                let Some(target) = target else {
                    if let Err(e) = source.idle() {
                        m.lock().unwrap().failure = Some(e.to_string());
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                    next = Instant::now();
                    continue;
                };
                let period = Duration::from_secs_f64(1.0 / fps as f64);
                std::thread::sleep(next.saturating_duration_since(Instant::now()));
                next += period;
                let force_key = key | k.swap(false, Ordering::AcqRel);
                let captured = match source.capture(&target, fps, force_key) {
                    Ok(Some(frame)) => frame,
                    Ok(None) => {
                        if force_key {
                            k.store(true, Ordering::Release);
                        }
                        continue;
                    }
                    Err(e) => {
                        m.lock().unwrap().failure = Some(e.to_string());
                        break;
                    }
                };
                let capture = captured.capture;
                let (w, h) = target.size.device();
                let cfg = (w, h, bps, fps);
                let mut encode_ms = 0.0;
                let result = (|| -> anyhow::Result<Vec<u8>> {
                    if (config.0, config.1, config.3) != (w, h, fps) {
                        let (new, warning) = Selected::new(
                            if hardware_disabled {
                                "software"
                            } else {
                                &preference
                            },
                            w,
                            h,
                            bps,
                            fps,
                        )?;
                        encoder = new;
                        if let Some(warning) = warning {
                            m.lock().unwrap().warnings.push(warning);
                        }
                        hardware_disabled = encoder.name() == "software/openh264";
                        config = cfg;
                    } else if config.2 != bps {
                        if let Err(e) = encoder.set_bitrate(bps) {
                            m.lock().unwrap().warnings.push(format!(
                                "Hardware bitrate change failed: {e:#}; falling back to software"
                            ));
                            encoder.fallback()?;
                            encoder.set_bitrate(bps)?;
                            hardware_disabled = true;
                        }
                        config.2 = bps;
                    }
                    let bgra = captured.pixels;
                    let encode_start = Instant::now();
                    let mut result = encoder.encode(&bgra, w as usize, h as usize, force_key);
                    if result.is_err() && !hardware_disabled {
                        m.lock().unwrap().warnings.push(format!(
                            "Hardware encode failed: {:#}; falling back to software",
                            result.as_ref().unwrap_err()
                        ));
                        encoder.fallback()?;
                        hardware_disabled = true;
                        result = encoder.encode(&bgra, w as usize, h as usize, true);
                    }
                    *name.lock().unwrap() = encoder.name().to_string();
                    encode_ms = encode_start.elapsed().as_secs_f64() * 1000.0;
                    result
                })();
                let mut stats = m.lock().unwrap();
                stats.captured += 1;
                match result {
                    Ok(data) if !data.is_empty() => {
                        encoded_id += 1;
                        stats.encoded += 1;
                        stats.bytes += data.len() as u64;
                        if stats.encode_ms.len() < 1200 {
                            stats.encode_ms.push(encode_ms);
                            stats
                                .capture_encode_ms
                                .push(capture.elapsed().as_secs_f64() * 1000.0);
                        }
                        // Publication checks the epoch again: resize/activate cannot publish old frames after their response.
                        let current = s.lock().unwrap();
                        if current.epoch == epoch && !current.stop {
                            tx.send_replace(Some(Frame {
                                data: Bytes::from(data),
                                capture,
                                duration: period,
                                epoch,
                                // Only a missing encoded access unit breaks the
                                // prediction chain. Capture skips do not.
                                id: encoded_id,
                            }));
                        } else {
                            stats.dropped += 1;
                        }
                    }
                    Ok(_) => stats.dropped += 1,
                    Err(e) => {
                        stats.failure = Some(e.to_string());
                        break;
                    }
                }
                if Instant::now() > next {
                    let missed = (Instant::now().duration_since(next).as_secs_f64()
                        / period.as_secs_f64())
                    .floor() as u64
                        + 1;
                    stats.dropped += missed;
                    next = Instant::now() + period;
                }
            }
        });
        Self {
            #[cfg(windows)]
            browser,
            state,
            metrics,
            frames,
            keyframe,
            encoder_name,
            worker: Some(worker),
        }
    }
    pub fn stop(&mut self) {
        self.state.lock().unwrap().stop = true;
    }
    pub async fn join(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = tokio::task::spawn_blocking(move || worker.join()).await;
        }
    }
}
pub fn render(t: &Target, frame: u64, seconds: f64) -> Vec<u8> {
    let (w, h) = t.size.device();
    let mut canvas = Canvas {
        w: w as usize,
        h: h as usize,
        pixels: vec![0; w as usize * h as usize * 4],
    };
    for y in 0..h as usize {
        for x in 0..w as usize {
            let i = (y * w as usize + x) * 4;
            let grid = if x % 64 < 2 || y % 64 < 2 { 45 } else { 0 };
            canvas.pixels[i] = (x * 100 / w as usize) as u8 + grid;
            canvas.pixels[i + 1] = (y * 80 / h as usize) as u8 + grid;
            canvas.pixels[i + 2] = 30 + grid;
            canvas.pixels[i + 3] = 255;
        }
    }
    let scale = ((w / 640).clamp(1, 4)) as usize;
    let marker = ((seconds * 180.0) as usize) % (w as usize).max(1);
    canvas.rect(
        marker as i32,
        (h / 2) as i32,
        64 * scale,
        48 * scale,
        [20, 240, 255],
    );
    let (rx, ry, rw, rh) = (
        (w / 3) as i32,
        (h / 3) as i32,
        (w / 3) as usize,
        (h / 3) as usize,
    );
    canvas.rect(
        rx,
        ry,
        rw,
        rh,
        if t.input.pointer {
            [80, 120, 30]
        } else {
            [90, 65, 30]
        },
    );
    canvas.rect(
        0,
        0,
        w as usize,
        (100 * scale).min(h as usize),
        [18, 18, 18],
    );
    canvas.text(
        8,
        8,
        &format!(
            "TARGET {} GEN {} FRAME {} TIME {:.2}s",
            t.target_id, t.generation, frame, seconds
        ),
        scale,
    );
    canvas.text(
        8,
        22 * scale,
        &format!(
            "FOCUS {} BUTTONS {} KEY {} WHEEL {:.0}",
            t.input.focused, t.input.buttons, t.input.last_key, t.input.wheel
        ),
        scale,
    );
    canvas.text(8, 36 * scale, &format!("TEXT {}", t.input.text), scale);
    canvas.text(8, 50 * scale, &t.url, scale);
    let px = (t.input.x * t.size.dpr) as i32;
    let py = (t.input.y * t.size.dpr) as i32;
    if t.input.buttons != 0
        || t.input
            .click
            .is_some_and(|at| at.elapsed() < Duration::from_millis(250))
    {
        canvas.rect(px - 14, py - 14, 28, 28, [20, 20, 255]);
    }
    // Synthetic test indicator is content, not an exported OS cursor.
    canvas.rect(px - 4, py - 4, 9, 9, [255, 255, 255]);
    canvas.pixels
}
struct Canvas {
    w: usize,
    h: usize,
    pixels: Vec<u8>,
}
impl Canvas {
    fn rect(&mut self, x: i32, y: i32, w: usize, h: usize, c: [u8; 3]) {
        for yy in y.max(0) as usize..(y as i64 + h as i64).clamp(0, self.h as i64) as usize {
            for xx in x.max(0) as usize..(x as i64 + w as i64).clamp(0, self.w as i64) as usize {
                let i = (yy * self.w + xx) * 4;
                self.pixels[i..i + 3].copy_from_slice(&c);
            }
        }
    }
    fn text(&mut self, x: usize, y: usize, text: &str, scale: usize) {
        let visible: String = text
            .chars()
            .flat_map(|c| {
                if font8x8::BASIC_FONTS.get(c).is_some() {
                    c.to_string()
                } else {
                    format!("U+{:04X}", c as u32)
                }
                .chars()
                .collect::<Vec<_>>()
            })
            .collect();
        for (n, c) in visible.chars().enumerate().take(self.w / (8 * scale)) {
            if let Some(bits) = font8x8::BASIC_FONTS.get(c) {
                for (row, byte) in bits.iter().enumerate() {
                    for col in 0..8 {
                        if byte & (1 << col) != 0 {
                            self.rect(
                                (x + (n * 8 + col) * scale) as i32,
                                (y + row * scale) as i32,
                                scale,
                                scale,
                                [240, 240, 240],
                            );
                        }
                    }
                }
            }
        }
    }
}
