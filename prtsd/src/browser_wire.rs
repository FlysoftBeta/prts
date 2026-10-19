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

#[derive(Debug)]
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
}
