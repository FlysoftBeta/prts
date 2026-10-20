//! PRTS v2 window contract. v1 page streams remain a separate protocol.
use crate::browser_wire::{Decoder, Record, Size};
use anyhow::{Result, ensure};
use std::collections::BTreeSet;

pub const WINDOW_CREATE: u16 = 0x400;
pub const WINDOW_CLOSE: u16 = 0x401;
pub const WINDOW_LIST: u16 = 0x402;
pub const WINDOW_RESIZE: u16 = 0x403;
pub const WINDOW_ACTIVATE: u16 = 0x404;
pub const WINDOW_FRAME_CONFIG: u16 = 0x410;
pub const WINDOW_FRAME_REFRESH: u16 = 0x411;
pub const TAB_CREATE: u16 = 0x420;
pub const TAB_CLOSE: u16 = 0x421;
pub const TAB_ACTIVATE: u16 = 0x422;
pub const TAB_NAVIGATE: u16 = 0x423;
pub const WINDOW_RESULT: u16 = 0x480;
pub const WINDOW_LIST_RESULT: u16 = 0x481;
pub const TAB_RESULT: u16 = 0x482;
pub const WINDOW_CREATED: u16 = 0x490;
pub const WINDOW_CHANGED: u16 = 0x491;
pub const WINDOW_CLOSED: u16 = 0x492;
pub const WINDOW_SCENE: u16 = 0x493;
pub const WINDOW_CAPS: u16 = 0x494;
pub const WINDOW_CURSOR: u16 = 0x495;
pub const WHOLE_WINDOW: u32 = 4;
pub const POPUP_LAYERS: u32 = 8;
pub const PREMULTIPLIED_BGRA: u32 = 16;

#[derive(Clone, Debug, PartialEq)]
pub struct Tab {
    pub id: u64,
    pub window: u64,
    pub opener: u64,
    pub url: String,
    pub title: String,
    pub load: u8,
    pub active: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Window {
    pub id: u64,
    pub kind: u8,
    pub opener: u64,
    pub size: Size,
    pub generation: u64,
    pub active_tab: u64,
    pub input_active: bool,
    pub closing: bool,
    pub tabs: Vec<Tab>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct DipRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Layer {
    pub source: u64,
    pub parent: u64,
    pub generation: u64,
    pub role: u8,
    pub rect: DipRect,
    pub input_rect: DipRect,
    pub width: u32,
    pub height: u32,
    pub dpr: f64,
    pub z: i32,
    pub visible: bool,
    pub accepts_input: bool,
    pub format: u8,
    pub opacity: f64,
    pub corners: [f64; 4],
    pub shadow: u8,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Scene {
    pub window: u64,
    pub generation: u64,
    pub revision: u64,
    pub layers: Vec<Layer>,
}
fn number(d: &mut Decoder<'_>) -> Result<f64> {
    let n = f64::from_bits(d.u64()?);
    ensure!(n.is_finite(), "Nonfinite geometry");
    Ok(n)
}
fn rect(d: &mut Decoder<'_>) -> Result<DipRect> {
    let r = DipRect {
        x: number(d)?,
        y: number(d)?,
        width: number(d)?,
        height: number(d)?,
    };
    ensure!(r.width >= 0. && r.height >= 0., "Negative extent");
    Ok(r)
}
impl Scene {
    pub fn decode(payload: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(payload);
        let window = d.u64()?;
        let generation = d.u64()?;
        let revision = d.u64()?;
        let count = d.u32()?;
        ensure!(
            window != 0 && generation != 0 && revision != 0 && (1..=9).contains(&count),
            "Invalid scene identity/limit"
        );
        let mut ids = BTreeSet::from([window]);
        let mut layers = Vec::new();
        for _ in 0..count {
            let source = d.u64()?;
            let parent = d.u64()?;
            let generation = d.u64()?;
            let role = d.u8()?;
            let rect = rect(&mut d)?;
            let input_rect = self::rect(&mut d)?;
            let width = d.u32()?;
            let height = d.u32()?;
            let dpr = number(&mut d)?;
            let z = d.u32()? as i32;
            let visible = d.boolean()?;
            let accepts_input = d.boolean()?;
            let format = d.u8()?;
            let opacity = number(&mut d)?;
            let mut corners = [0.; 4];
            for c in &mut corners {
                *c = number(&mut d)?;
            }
            let shadow = d.u8()?;
            ensure!(
                source != 0 && ids.insert(source) && generation != 0 && (1..=8).contains(&role),
                "Invalid/aliased layer"
            );
            ensure!(
                width > 0
                    && width <= 16384
                    && height > 0
                    && height <= 16384
                    && u64::from(width) * u64::from(height) <= 64_000_000
                    && (0.5..=4.).contains(&dpr),
                "Invalid layer size"
            );
            ensure!(
                (0.0..=1.).contains(&opacity)
                    && corners.iter().all(|c| *c >= 0.)
                    && shadow <= 2
                    && format == if role == 1 { 1 } else { 2 },
                "Invalid decoration/format"
            );
            layers.push(Layer {
                source,
                parent,
                generation,
                role,
                rect,
                input_rect,
                width,
                height,
                dpr,
                z,
                visible,
                accepts_input,
                format,
                opacity,
                corners,
                shadow,
            });
        }
        d.finish()?;
        ensure!(
            layers
                .iter()
                .filter(|l| l.role == 1 && l.parent == 0)
                .count()
                == 1,
            "Missing/duplicate base"
        );
        for l in &layers {
            let mut current = l;
            let mut visited = BTreeSet::new();
            while current.parent != 0 {
                ensure!(visited.insert(current.source), "Cyclic layer owners");
                current = layers
                    .iter()
                    .find(|p| p.source == current.parent)
                    .ok_or_else(|| anyhow::anyhow!("Unknown layer owner"))?;
            }
            ensure!(current.role == 1, "Disconnected layer owner");
        }
        Ok(Self {
            window,
            generation,
            revision,
            layers,
        })
    }
}
fn tab(d: &mut Decoder<'_>) -> Result<Tab> {
    let t = Tab {
        id: d.u64()?,
        window: d.u64()?,
        opener: d.u64()?,
        url: d.string()?,
        title: d.string()?,
        load: d.u8()?,
        active: d.boolean()?,
    };
    ensure!(
        t.id != 0 && t.window != 0 && t.id != t.window && t.load <= 3,
        "Invalid tab"
    );
    ensure!(
        !t.url.contains('\0') && !t.title.contains('\0'),
        "NUL tab metadata"
    );
    Ok(t)
}
fn window(d: &mut Decoder<'_>) -> Result<Window> {
    let id = d.u64()?;
    let kind = d.u8()?;
    let opener = d.u64()?;
    let size = Size {
        width: d.u32()?,
        height: d.u32()?,
        dpr: f64::from_bits(d.u64()?),
    };
    size.validate()?;
    let generation = d.u64()?;
    let active_tab = d.u64()?;
    let input_active = d.boolean()?;
    let closing = d.boolean()?;
    let count = d.u32()?;
    ensure!(
        id != 0 && generation != 0 && (1..=3).contains(&kind) && count <= 256,
        "Invalid window"
    );
    let mut tabs = Vec::new();
    let mut ids = BTreeSet::from([id]);
    for _ in 0..count {
        let t = tab(d)?;
        ensure!(
            t.window == id && ids.insert(t.id),
            "Cross-owner or duplicate tab"
        );
        ensure!(t.active == (active_tab == t.id), "Inconsistent active tab");
        tabs.push(t);
    }
    ensure!(
        (active_tab == 0 && tabs.is_empty()) || tabs.iter().any(|t| t.id == active_tab),
        "Unknown active tab"
    );
    Ok(Window {
        id,
        kind,
        opener,
        size,
        generation,
        active_tab,
        input_active,
        closing,
        tabs,
    })
}
impl Window {
    pub fn decode(payload: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(payload);
        let w = window(&mut d)?;
        d.finish()?;
        Ok(w)
    }
    pub fn list(payload: &[u8]) -> Result<Vec<Self>> {
        let mut d = Decoder::new(payload);
        let count = d.u32()?;
        ensure!(count <= 256, "Window list exceeds hard limit");
        let mut windows = Vec::new();
        let mut ids = BTreeSet::new();
        for _ in 0..count {
            let w = window(&mut d)?;
            ensure!(ids.insert(w.id), "Duplicate window");
            for t in &w.tabs {
                ensure!(ids.insert(t.id), "Aliased identity");
            }
            windows.push(w);
        }
        ensure!(
            windows.iter().filter(|w| w.input_active).count() <= 1,
            "Multiple input windows"
        );
        d.finish()?;
        Ok(windows)
    }
}
impl Tab {
    pub fn decode(payload: &[u8]) -> Result<Self> {
        let mut d = Decoder::new(payload);
        let t = tab(&mut d)?;
        d.finish()?;
        Ok(t)
    }
}
/// Bootstrap is always 1.0; exact 2.0 forbids a silent page-stream downgrade.
pub fn hello(id: u64, luid: u64, modes: u32, inflight: u32) -> Record {
    let mut payload = Vec::new();
    for n in [2u16, 0, 2, 0] {
        payload.extend(n.to_le_bytes());
    }
    payload.extend(std::process::id().to_le_bytes());
    payload.extend(luid.to_le_bytes());
    payload.extend(modes.to_le_bytes());
    payload.extend(inflight.to_le_bytes());
    Record {
        kind: crate::browser_wire::HELLO,
        id,
        payload,
    }
}
pub fn check_ack(record: &Record, required_features: u32) -> Result<(u64, u32)> {
    ensure!(
        record.kind == crate::browser_wire::HELLO_ACK,
        "Expected HELLO_ACK"
    );
    let mut d = Decoder::new(&record.payload);
    ensure!(
        d.u16()? == 2 && d.u16()? == 0,
        "Whole-window protocol downgrade"
    );
    ensure!(d.u32()? != 0, "Zero browser PID");
    let luid = d.u64()?;
    let modes = d.u32()?;
    let inflight = d.u32()?;
    ensure!(
        modes & 2 != 0 && modes & !3 == 0 && (1..=3).contains(&inflight),
        "Invalid transport capabilities"
    );
    for _ in 0..3 {
        ensure!(d.u32()? != 0, "Zero capture limit");
    }
    ensure!(
        d.u32()? & required_features == required_features,
        "Required window capabilities missing"
    );
    d.finish()?;
    Ok((luid, modes))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn one_window(owner: u64, tab_id: u64, membership: u64) -> Vec<u8> {
        let mut p = owner.to_le_bytes().to_vec();
        p.push(1);
        p.extend(0u64.to_le_bytes());
        Size {
            width: 960,
            height: 640,
            dpr: 1.25,
        }
        .encode(&mut p)
        .unwrap();
        p.extend(1u64.to_le_bytes());
        p.extend(tab_id.to_le_bytes());
        p.extend([1, 0]);
        p.extend(1u32.to_le_bytes());
        p.extend(tab_id.to_le_bytes());
        p.extend(membership.to_le_bytes());
        p.extend(0u64.to_le_bytes());
        crate::browser_wire::string("about:blank", &mut p).unwrap();
        crate::browser_wire::string("test", &mut p).unwrap();
        p.extend([2, 1]);
        p
    }
    #[test]
    fn window_membership_and_identity_are_authority() -> Result<()> {
        let p = one_window(1, 3, 1);
        assert_eq!(Window::decode(&p)?.tabs[0].id, 3);
        for n in 0..p.len() {
            assert!(
                Window::decode(&p[..n]).is_err(),
                "Accepted truncation at {n}"
            );
        }
        assert!(Window::decode(&one_window(1, 1, 1)).is_err());
        assert!(Window::decode(&one_window(1, 3, 2)).is_err());
        let mut trailing = p.clone();
        trailing.push(0);
        assert!(Window::decode(&trailing).is_err());
        let mut list = 2u32.to_le_bytes().to_vec();
        list.extend(&p);
        list.extend(&p);
        assert!(Window::list(&list).is_err());
        let mut nonfinite = p;
        nonfinite[25..33].copy_from_slice(&f64::NAN.to_le_bytes());
        assert!(Window::decode(&nonfinite).is_err());
        Ok(())
    }
    #[test]
    fn envelope_isolation_and_no_downgrade() -> Result<()> {
        let r = Record {
            kind: WINDOW_LIST,
            id: 3,
            payload: vec![],
        };
        let bytes = r.encode_version(2)?;
        assert!(Record::read(&mut &bytes[..]).is_err());
        assert_eq!(Record::read_version(&mut &bytes[..], 2)?.kind, WINDOW_LIST);
        assert!(Record::read_version(&mut &r.encode()?[..], 2).is_err());
        let mut ack = Record {
            kind: crate::browser_wire::HELLO_ACK,
            id: 1,
            payload: vec![0; 44],
        };
        ack.payload[0] = 1;
        assert!(check_ack(&ack, WHOLE_WINDOW).is_err());
        for n in 0..64 {
            assert!(Window::decode(&vec![0; n]).is_err());
        }
        assert!(Window::list(&257u32.to_le_bytes()).is_err());
        Ok(())
    }
}
