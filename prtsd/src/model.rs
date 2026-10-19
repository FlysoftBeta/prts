use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, time::Instant};

pub const MAX_SAFE: u64 = 9_007_199_254_740_991;
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, Clone, Serialize)]
pub struct Error {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}
impl Error {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
        }
    }
}
impl From<webrtc::error::Error> for Error {
    fn from(_: webrtc::error::Error) -> Self {
        Self::new(
            "invalid_state",
            "WebRTC operation failed (check signaling state/SDP)",
        )
    }
}
pub fn invalid(message: impl Into<String>) -> Error {
    Error::new("invalid_argument", message)
}
pub fn string<'a>(v: &'a Value, k: &str) -> Result<&'a str> {
    v.get(k)
        .and_then(Value::as_str)
        .filter(|s| s.len() <= 65536)
        .ok_or_else(|| invalid(format!("{k} must be a string of at most 64 KiB")))
}
pub fn boolean(v: &Value, k: &str, default: bool) -> Result<bool> {
    match v.get(k) {
        None => Ok(default),
        Some(v) => v
            .as_bool()
            .ok_or_else(|| invalid(format!("{k} must be boolean"))),
    }
}
pub fn integer(v: &Value, k: &str, lo: u64, hi: u64) -> Result<u64> {
    v.get(k)
        .and_then(Value::as_u64)
        .filter(|n| *n >= lo && *n <= hi.min(MAX_SAFE))
        .ok_or_else(|| invalid(format!("{k} is outside {lo}..{hi}")))
}
pub fn number(v: &Value, k: &str, lo: f64, hi: f64) -> Result<f64> {
    v.get(k)
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n >= lo && *n <= hi)
        .ok_or_else(|| invalid(format!("{k} is outside {lo}..{hi}")))
}
pub fn url(v: &str) -> Result<String> {
    if v == "about:blank" {
        return Ok(v.into());
    }
    let u = url::Url::parse(v)
        .map_err(|_| invalid("URL must be absolute http/https or about:blank"))?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        return Err(invalid("URL scheme is not permitted"));
    }
    Ok(v.into())
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Size {
    pub width: u32,
    pub height: u32,
    #[serde(default = "one")]
    pub dpr: f64,
}
fn one() -> f64 {
    1.0
}
impl Default for Size {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 720,
            dpr: 1.0,
        }
    }
}
impl Size {
    pub fn parse(v: &Value) -> Result<Self> {
        let s: Self = serde_json::from_value(v.clone()).map_err(|_| invalid("invalid size"))?;
        let (w, h) = s.device();
        // Phase 1 software limits bound allocation and encode cost. Odd sizes are padded in I420.
        if s.width == 0
            || s.height == 0
            || s.width > 4096
            || s.height > 4096
            || !s.dpr.is_finite()
            || !(0.5..=4.0).contains(&s.dpr)
            || w < 16
            || h < 16
            || w > 4096
            || h > 4096
            || w * h > 8_294_400
            || w % 2 != 0
            || h % 2 != 0
        {
            return Err(invalid(
                "software requires even device dimensions, 16..4096 per axis, at most 8294400 device pixels",
            ));
        }
        Ok(s)
    }
    pub fn device(self) -> (u32, u32) {
        (
            (self.width as f64 * self.dpr).ceil() as u32,
            (self.height as f64 * self.dpr).ceil() as u32,
        )
    }
}
#[derive(Clone, Serialize)]
pub struct Options {
    pub source: String,
    pub initial_url: String,
    pub size: Size,
    pub encoder: String,
    pub bitrate_caps: Caps,
    pub isolation_profile: String,
    pub fps: u32,
}
#[derive(Clone, Serialize)]
pub struct Caps {
    pub min_bps: u32,
    pub max_bps: u32,
}
impl Options {
    pub fn parse(v: &Value) -> Result<Self> {
        if !v.is_object() {
            return Err(invalid("options must be an object"));
        }
        let source = v
            .get("source")
            .map_or(Ok("synthetic"), |_| string(v, "source"))?;
        if source != "synthetic" {
            return Err(Error::new(
                "unsupported",
                "Chromium source is not implemented; use synthetic",
            ));
        }
        let encoder = v
            .get("encoder")
            .map_or(Ok("auto"), |_| string(v, "encoder"))?;
        if !matches!(encoder, "auto" | "qsv" | "software") {
            return Err(Error::new(
                "unsupported",
                "Encoder must be auto, qsv, or software",
            ));
        }
        let profile = v
            .get("isolation_profile")
            .map_or(Ok("job"), |_| string(v, "isolation_profile"))?;
        if profile != "job" {
            return Err(Error::new(
                "unsupported",
                "Only job policy is supported (synthetic creates no process)",
            ));
        }
        let caps = v.get("bitrate_caps").cloned().unwrap_or(json!({}));
        if !caps.is_object() {
            return Err(invalid("bitrate_caps must be an object"));
        }
        let min = if caps.get("min_bps").is_some() {
            integer(&caps, "min_bps", 1, 100_000_000)? as u32
        } else {
            100_000
        };
        let max = if caps.get("max_bps").is_some() {
            integer(&caps, "max_bps", min as u64, 100_000_000)? as u32
        } else {
            4_000_000
        };
        if min > max {
            return Err(invalid("min_bps exceeds max_bps"));
        }
        Ok(Self {
            source: source.into(),
            initial_url: url(v
                .get("initial_url")
                .map_or(Ok("about:blank"), |_| string(v, "initial_url"))?)?,
            size: v
                .get("size")
                .map(Size::parse)
                .transpose()?
                .unwrap_or_default(),
            encoder: encoder.into(),
            bitrate_caps: Caps {
                min_bps: min,
                max_bps: max,
            },
            isolation_profile: profile.into(),
            fps: if v.get("fps").is_some() {
                integer(v, "fps", 1, 60)? as u32
            } else {
                30
            },
        })
    }
}
#[derive(Clone, Serialize)]
pub struct Target {
    pub target_id: String,
    pub opener_target_id: Option<String>,
    pub kind: &'static str,
    pub load: &'static str,
    pub size: Size,
    pub url: String,
    pub title: String,
    pub generation: String,
    pub active: bool,
    #[serde(skip)]
    pub input: Input,
    #[serde(skip)]
    pub complete_at: Option<Instant>,
}
impl Target {
    pub fn new(id: u64, url: String, size: Size, active: bool) -> Self {
        Self {
            target_id: id.to_string(),
            opener_target_id: None,
            kind: "tab",
            load: "loading",
            size,
            title: format!("PRTS synthetic: {url}"),
            url,
            generation: "1".into(),
            active,
            input: Input::default(),
            complete_at: Some(Instant::now() + std::time::Duration::from_millis(15)),
        }
    }
    pub fn bump(&mut self) {
        self.generation = (self.generation.parse::<u64>().unwrap() + 1).to_string();
        self.input.release();
    }
}
#[derive(Clone, Default)]
pub struct Input {
    pub x: f64,
    pub y: f64,
    pub buttons: u32,
    pub focused: bool,
    pub text: String,
    pub last_key: String,
    pub click: Option<Instant>,
    pub pointer: bool,
    pub keys: BTreeMap<String, bool>,
    pub wheel: f64,
}
impl Input {
    pub fn release(&mut self) {
        self.buttons = 0;
        self.keys.clear();
        self.focused = false;
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct Network {
    pub bitrate_cap_bps: Option<u32>,
    pub added_latency_ms: u32,
    pub loss_percent: f64,
    pub seed: u32,
}
impl Default for Network {
    fn default() -> Self {
        Self {
            bitrate_cap_bps: None,
            added_latency_ms: 0,
            loss_percent: 0.0,
            seed: 1,
        }
    }
}
impl Network {
    pub fn parse(v: &Value) -> Result<Self> {
        let n: Self = serde_json::from_value(v.clone())
            .map_err(|_| invalid("all NetworkSettings fields are required"))?;
        if v.get("bitrate_cap_bps").is_none()
            || n.bitrate_cap_bps.is_some_and(|x| x == 0 || x > 100_000_000)
            || n.added_latency_ms > 2000
            || !n.loss_percent.is_finite()
            || !(0.0..=100.0).contains(&n.loss_percent)
        {
            return Err(invalid("invalid network settings"));
        }
        Ok(n)
    }
}
