//! Protocol section 2. Shared with the standalone real-browser acceptance driver.
use anyhow::{Result, bail, ensure};
use std::io::{Read, Write};

pub const MAX_RECORD: usize = 1_048_576;
pub const HELLO: u16 = 1;
pub const HELLO_ACK: u16 = 2;
pub const ERROR: u16 = 3;
pub const PING: u16 = 4;
pub const PONG: u16 = 5;
pub const SHUTDOWN: u16 = 6;
pub const OK: u16 = 7;
pub const FAULT: u16 = 8;
pub const CREATE: u16 = 0x100;
pub const CLOSE: u16 = 0x101;
pub const NAVIGATE: u16 = 0x102;
pub const LIST: u16 = 0x103;
pub const RESIZE: u16 = 0x104;
pub const ACTIVATE: u16 = 0x105;
pub const TARGET_RESULT: u16 = 0x180;
pub const LIST_RESULT: u16 = 0x181;
pub const CREATED: u16 = 0x190;
pub const CLOSED: u16 = 0x191;
pub const CHANGED: u16 = 0x192;
pub const CURSOR_CHANGED: u16 = 0x193;

#[derive(Clone, Debug, PartialEq)]
pub struct Cursor {
    pub shape: u8,
    pub hot_x: u32,
    pub hot_y: u32,
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
}
#[derive(Clone, Debug)]
pub struct InputHeader {
    pub target: u64,
    pub generation: u64,
    pub sequence: u64,
    pub modifiers: u32,
}
#[derive(Clone, Debug)]
pub struct Key {
    pub vk: u32,
    pub code: String,
    pub key: String,
    pub text: String,
    pub location: u8,
    pub repeat: bool,
    pub is_system: bool,
}
#[derive(Clone, Debug)]
pub enum Input {
    MouseMove {
        x: f64,
        y: f64,
        buttons: u32,
    },
    MouseDown {
        x: f64,
        y: f64,
        button: u8,
        buttons: u32,
        click_count: u8,
    },
    MouseUp {
        x: f64,
        y: f64,
        button: u8,
        buttons: u32,
        click_count: u8,
    },
    Wheel {
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
        buttons: u32,
    },
    KeyDown(Key),
    KeyUp(Key),
    Char(String),
    Focus(bool),
}
impl Input {
    pub fn record(&self, header: &InputHeader) -> Result<Record> {
        ensure!(
            header.target != 0
                && header.generation != 0
                && header.sequence != 0
                && header.modifiers <= 63,
            "Invalid input header"
        );
        let mut p = Vec::new();
        p.extend(header.target.to_le_bytes());
        p.extend(header.generation.to_le_bytes());
        p.extend(header.sequence.to_le_bytes());
        p.extend(header.modifiers.to_le_bytes());
        let kind = match self {
            Self::MouseMove { x, y, buttons }
            | Self::MouseDown { x, y, buttons, .. }
            | Self::MouseUp { x, y, buttons, .. }
            | Self::Wheel { x, y, buttons, .. } => {
                ensure!(
                    x.is_finite() && y.is_finite() && *buttons <= 31,
                    "Invalid mouse values"
                );
                p.extend(x.to_le_bytes());
                p.extend(y.to_le_bytes());
                let kind = match self {
                    Self::MouseMove { .. } => 0x200,
                    Self::MouseDown { .. } => 0x201,
                    Self::MouseUp { .. } => 0x202,
                    Self::Wheel {
                        delta_x, delta_y, ..
                    } => {
                        ensure!(
                            delta_x.is_finite() && delta_y.is_finite(),
                            "Invalid wheel delta"
                        );
                        p.extend(delta_x.to_le_bytes());
                        p.extend(delta_y.to_le_bytes());
                        0x203
                    }
                    _ => unreachable!(),
                };
                if let Self::MouseDown {
                    button,
                    click_count,
                    ..
                }
                | Self::MouseUp {
                    button,
                    click_count,
                    ..
                } = self
                {
                    ensure!(
                        (1..=5).contains(button) && (1..=3).contains(click_count),
                        "Invalid mouse transition"
                    );
                    p.push(*button);
                }
                p.extend(buttons.to_le_bytes());
                if let Self::MouseDown { click_count, .. } | Self::MouseUp { click_count, .. } =
                    self
                {
                    p.push(*click_count);
                }
                kind
            }
            Self::KeyDown(k) | Self::KeyUp(k) => {
                ensure!(
                    k.vk <= 255
                        && k.location <= 3
                        && (!k.code.is_empty() || k.key == "Unidentified"),
                    "Invalid key"
                );
                ensure!(!k.code.contains('\0') && !k.key.contains('\0'), "NUL key");
                if matches!(self, Self::KeyUp(_)) {
                    ensure!(k.text.is_empty() && !k.repeat, "Invalid key up");
                }
                p.extend(k.vk.to_le_bytes());
                string(&k.code, &mut p)?;
                string(&k.key, &mut p)?;
                input_text(&k.text, &mut p)?;
                p.extend([k.location, k.repeat as u8, k.is_system as u8]);
                if matches!(self, Self::KeyDown(_)) {
                    0x204
                } else {
                    0x205
                }
            }
            Self::Char(text) => {
                input_text(text, &mut p)?;
                0x206
            }
            Self::Focus(focused) => {
                p.push(*focused as u8);
                0x207
            }
        };
        Ok(Record {
            kind,
            id: 0,
            payload: p,
        })
    }
}
fn input_text(text: &str, p: &mut Vec<u8>) -> Result<()> {
    ensure!(
        text.len() <= 4096 && !text.contains('\0'),
        "Invalid input text"
    );
    string(text, p)
}

#[derive(Clone, Debug, PartialEq)]
pub struct Size {
    pub width: u32,
    pub height: u32,
    pub dpr: f64,
}
impl Size {
    pub fn validate(&self) -> Result<()> {
        ensure!(
            (1..=8192).contains(&self.width) && (1..=8192).contains(&self.height),
            "CSS size out of range"
        );
        ensure!(
            self.dpr.is_finite() && (0.5..=4.).contains(&self.dpr),
            "DPR out of range"
        );
        let w = (self.width as f64 * self.dpr).ceil();
        let h = (self.height as f64 * self.dpr).ceil();
        ensure!(
            w <= 16384. && h <= 16384. && w * h <= 64_000_000.,
            "Device size out of range"
        );
        Ok(())
    }
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<()> {
        self.validate()?;
        out.extend(self.width.to_le_bytes());
        out.extend(self.height.to_le_bytes());
        out.extend(self.dpr.to_le_bytes());
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub id: u64,
    pub kind: u8,
    pub opener: u64,
    pub url: String,
    pub title: String,
    pub load: u8,
    pub size: Size,
    pub generation: u64,
    pub active: bool,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    Created(Target),
    Cursor {
        target: u64,
        generation: u64,
        cursor: Cursor,
    },
    Changed(Target),
    Closed {
        target: u64,
        reason: String,
    },
    Fault {
        code: u32,
        message: String,
        fatal: bool,
    },
}

#[derive(Clone, Debug)]
pub struct Record {
    pub kind: u16,
    pub id: u64,
    pub payload: Vec<u8>,
}
impl Record {
    pub fn encode(&self) -> Result<Vec<u8>> {
        ensure!(self.payload.len() <= MAX_RECORD - 20, "Record too large");
        let mut out = Vec::with_capacity(24 + self.payload.len());
        out.extend((20u32 + self.payload.len() as u32).to_le_bytes());
        out.extend(b"PRTS");
        out.extend(1u16.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out.extend(self.kind.to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out.extend(self.id.to_le_bytes());
        out.extend(&self.payload);
        Ok(out)
    }
    pub fn read(reader: &mut impl Read) -> Result<Self> {
        let mut prefix = [0; 4];
        reader.read_exact(&mut prefix)?;
        let len = u32::from_le_bytes(prefix) as usize;
        ensure!(
            (20..=MAX_RECORD).contains(&len),
            "Invalid record length {len}"
        );
        let mut bytes = vec![0; len];
        reader.read_exact(&mut bytes)?;
        let mut d = Decoder::new(&bytes);
        ensure!(
            d.u32()? == 0x53545250 && d.u16()? == 1 && d.u16()? == 0,
            "Invalid magic/version"
        );
        let kind = d.u16()?;
        ensure!(d.u16()? == 0, "Nonzero flags");
        let id = d.u64()?;
        ensure!(
            matches!(kind, 1..=8 | 0x100..=0x105 | 0x180..=0x181 | 0x190..=0x193 | 0x200..=0x207 | 0x300..=0x301 | 0x310..=0x311 | 0x320..=0x321),
            "Unknown record type"
        );
        Ok(Self {
            kind,
            id,
            payload: bytes[20..].to_vec(),
        })
    }
    pub fn write(&self, writer: &mut impl Write) -> Result<()> {
        writer.write_all(&self.encode()?)?;
        Ok(())
    }
    pub fn event(&self) -> Result<Event> {
        ensure!(self.id == 0, "Notification request ID must be zero");
        let mut d = Decoder::new(&self.payload);
        let event = match self.kind {
            CREATED => Event::Created(d.target()?),
            CHANGED => Event::Changed(d.target()?),
            CLOSED => {
                let target = d.u64()?;
                ensure!(target != 0, "Zero closed ID");
                Event::Closed {
                    target,
                    reason: d.string()?,
                }
            }
            CURSOR_CHANGED => {
                let target = d.u64()?;
                let generation = d.u64()?;
                let shape = d.u8()?;
                let hot_x = d.u32()?;
                let hot_y = d.u32()?;
                let width = d.u32()?;
                let height = d.u32()?;
                let n = d.u32()? as usize;
                ensure!(
                    target != 0
                        && generation != 0
                        && shape <= 8
                        && n <= 32768
                        && n <= d.data.len() - d.pos,
                    "Invalid cursor"
                );
                let png = d.data[d.pos..d.pos + n].to_vec();
                d.pos += n;
                if shape == 8 {
                    ensure!(
                        (1..=128).contains(&width)
                            && (1..=128).contains(&height)
                            && hot_x < width
                            && hot_y < height
                            && png.starts_with(b"\x89PNG\r\n\x1a\n"),
                        "Invalid custom cursor"
                    );
                } else {
                    ensure!(
                        width == 0 && height == 0 && hot_x == 0 && hot_y == 0 && png.is_empty(),
                        "Invalid built-in cursor"
                    );
                }
                Event::Cursor {
                    target,
                    generation,
                    cursor: Cursor {
                        shape,
                        hot_x,
                        hot_y,
                        width,
                        height,
                        png,
                    },
                }
            }
            FAULT => {
                let code = d.u32()?;
                ensure!((1..=10).contains(&code), "Invalid fault code");
                Event::Fault {
                    code,
                    message: d.string()?,
                    fatal: d.boolean()?,
                }
            }
            _ => bail!("Unexpected event type {} in M1", self.kind),
        };
        d.finish()?;
        Ok(event)
    }
}
pub fn string(value: &str, out: &mut Vec<u8>) -> Result<()> {
    ensure!(value.len() <= 65536, "String too long");
    out.extend((value.len() as u32).to_le_bytes());
    out.extend(value.as_bytes());
    Ok(())
}
pub struct Decoder<'a> {
    data: &'a [u8],
    pos: usize,
}
impl<'a> Decoder<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn bytes<const N: usize>(&mut self) -> Result<[u8; N]> {
        let end = self
            .pos
            .checked_add(N)
            .ok_or_else(|| anyhow::anyhow!("Overflow"))?;
        ensure!(end <= self.data.len(), "Truncated payload");
        let out = self.data[self.pos..end].try_into()?;
        self.pos = end;
        Ok(out)
    }
    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.bytes::<1>()?[0])
    }
    pub fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.bytes()?))
    }
    pub fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.bytes()?))
    }
    pub fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.bytes()?))
    }
    pub fn boolean(&mut self) -> Result<bool> {
        let v = self.u8()?;
        ensure!(v <= 1, "Invalid boolean");
        Ok(v != 0)
    }
    pub fn string(&mut self) -> Result<String> {
        let n = self.u32()? as usize;
        ensure!(
            n <= 65536 && n <= self.data.len() - self.pos,
            "Invalid string length"
        );
        let s = std::str::from_utf8(&self.data[self.pos..self.pos + n])?.to_owned();
        self.pos += n;
        Ok(s)
    }
    pub fn target(&mut self) -> Result<Target> {
        let id = self.u64()?;
        let kind = self.u8()?;
        let opener = self.u64()?;
        let url = self.string()?;
        let title = self.string()?;
        let load = self.u8()?;
        let size = Size {
            width: self.u32()?,
            height: self.u32()?,
            dpr: f64::from_le_bytes(self.bytes()?),
        };
        size.validate()?;
        let generation = self.u64()?;
        let active = self.boolean()?;
        ensure!(
            id != 0 && generation != 0 && matches!(kind, 1 | 2) && load <= 3,
            "Invalid target fields"
        );
        Ok(Target {
            id,
            kind,
            opener,
            url,
            title,
            load,
            size,
            generation,
            active,
        })
    }
    pub fn finish(&self) -> Result<()> {
        ensure!(self.pos == self.data.len(), "Trailing payload bytes");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct ByteReader<'a>(&'a [u8]);
    impl Read for ByteReader<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            let n = out.len().min(self.0.len()).min(1);
            out[..n].copy_from_slice(&self.0[..n]);
            self.0 = &self.0[n..];
            Ok(n)
        }
    }
    #[test]
    fn partial_records_and_bounds() -> Result<()> {
        let bytes = Record {
            kind: PING,
            id: 3,
            payload: 123u64.to_le_bytes().to_vec(),
        }
        .encode()?;
        let record = Record::read(&mut ByteReader(&bytes))?;
        assert_eq!(record.id, 3);
        assert_eq!(record.kind, PING);
        assert!(Record::read(&mut &bytes[..bytes.len() - 1]).is_err());
        assert!(Record::read(&mut &19u32.to_le_bytes()[..]).is_err());
        assert!(Record::read(&mut &(MAX_RECORD as u32 + 1).to_le_bytes()[..]).is_err());
        let mut bad = bytes;
        bad[14] = 1;
        assert!(Record::read(&mut &bad[..]).is_err());
        Ok(())
    }
    #[test]
    fn invalid_payloads() {
        assert!(Decoder::new(&[2]).boolean().is_err());
        assert!(Decoder::new(&[1]).finish().is_err());
        assert!(
            Size {
                width: 8192,
                height: 8192,
                dpr: 4.
            }
            .validate()
            .is_err()
        );
        let mut payload = 0u32.to_le_bytes().to_vec();
        payload.extend(0u32.to_le_bytes());
        payload.push(0);
        assert!(
            Record {
                kind: FAULT,
                id: 0,
                payload
            }
            .event()
            .is_err()
        );
    }
    #[test]
    fn input_rejects_bad_values_and_preserves_unicode() -> Result<()> {
        let header = InputHeader {
            target: 1,
            generation: 2,
            sequence: 3,
            modifiers: 0,
        };
        assert!(
            Input::MouseMove {
                x: f64::NAN,
                y: 0.,
                buttons: 0
            }
            .record(&header)
            .is_err()
        );
        assert!(Input::Char("bad\0text".into()).record(&header).is_err());
        assert!(Input::Char("x".repeat(4097)).record(&header).is_err());
        let text = "a\u{1f600}".to_owned();
        let record = Input::Char(text.clone()).record(&header)?;
        let mut decoder = Decoder::new(&record.payload);
        assert_eq!(
            (
                decoder.u64()?,
                decoder.u64()?,
                decoder.u64()?,
                decoder.u32()?
            ),
            (1, 2, 3, 0)
        );
        assert_eq!(decoder.string()?, text);
        decoder.finish()?;
        let key = Key {
            vk: 65,
            code: "KeyA".into(),
            key: "a".into(),
            text: "a".into(),
            location: 0,
            repeat: false,
            is_system: false,
        };
        assert!(Input::KeyUp(key).record(&header).is_err());
        Ok(())
    }
    #[test]
    fn cursor_bounds_and_exact_consumption() -> Result<()> {
        let mut payload = 1u64.to_le_bytes().to_vec();
        payload.extend(2u64.to_le_bytes());
        payload.push(1);
        payload.extend([0; 20]);
        let mut record = Record {
            kind: CURSOR_CHANGED,
            id: 0,
            payload,
        };
        assert!(matches!(
            record.event()?,
            Event::Cursor {
                cursor: Cursor { shape: 1, .. },
                ..
            }
        ));
        record.payload.push(0);
        assert!(record.event().is_err());
        record.payload.pop();
        record.payload[17] = 1;
        assert!(record.event().is_err());
        record.payload[17] = 0;
        record.payload[16] = 8;
        assert!(record.event().is_err());
        record.payload[16] = 9;
        assert!(record.event().is_err());
        Ok(())
    }
}
