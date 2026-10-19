//! Blocking anonymous stdio workers; no Tokio stdin task that can hold shutdown open.
use serde_json::{Value, json};
use std::{
    io::{self, Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    time::Instant,
};
use tokio::sync::mpsc as async_mpsc;

pub const MAX_RECORD: usize = 4_194_304;
const MAX_QUEUE: usize = 8 * 1024 * 1024;
pub fn read_record(reader: &mut impl Read) -> io::Result<Option<Value>> {
    let mut header = [0; 4];
    match reader.read(&mut header[..1])? {
        0 => return Ok(None),
        _ => reader.read_exact(&mut header[1..])?,
    }
    let len = u32::from_le_bytes(header) as usize;
    if !(1..=MAX_RECORD).contains(&len) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid record length",
        ));
    }
    let mut data = vec![0; len];
    reader.read_exact(&mut data)?;
    let value: Value = serde_json::from_slice(&data)?;
    if !value.is_object() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "record must be an object",
        ));
    }
    Ok(Some(value))
}
pub fn write_record(writer: &mut impl Write, v: &Value) -> io::Result<()> {
    let data = serde_json::to_vec(v)?;
    writer.write_all(&(data.len() as u32).to_le_bytes())?;
    writer.write_all(&data)?;
    writer.flush()
}
struct Writer {
    sender: mpsc::SyncSender<Vec<u8>>,
    seq: u64,
}
#[derive(Clone)]
pub struct Output {
    inner: Arc<Mutex<Writer>>,
    bytes: Arc<AtomicUsize>,
    pub failed: Arc<AtomicBool>,
    start: Instant,
}
impl Output {
    pub fn start() -> Self {
        let (sender, receiver) = mpsc::sync_channel::<Vec<u8>>(1024);
        let bytes = Arc::new(AtomicUsize::new(0));
        let failed = Arc::new(AtomicBool::new(false));
        let (b, f) = (bytes.clone(), failed.clone());
        std::thread::spawn(move || {
            let stdout = io::stdout();
            let mut w = stdout.lock();
            for data in receiver {
                if w.write_all(&(data.len() as u32).to_le_bytes())
                    .and_then(|_| w.write_all(&data))
                    .and_then(|_| w.flush())
                    .is_err()
                {
                    f.store(true, Ordering::Release);
                    break;
                }
                b.fetch_sub(data.len() + 4, Ordering::AcqRel);
            }
        });
        Self {
            inner: Arc::new(Mutex::new(Writer { sender, seq: 0 })),
            bytes,
            failed,
            start: Instant::now(),
        }
    }
    fn enqueue(&self, w: &mut Writer, v: &Value, coalesce: bool) {
        if self.failed.load(Ordering::Acquire) {
            return;
        }
        let data = serde_json::to_vec(v).expect("JSON output");
        let n = data.len() + 4;
        if data.len() > MAX_RECORD || self.bytes.load(Ordering::Acquire) + n > MAX_QUEUE {
            if !coalesce {
                self.failed.store(true, Ordering::Release);
            }
            return;
        }
        self.bytes.fetch_add(n, Ordering::AcqRel);
        if w.sender.try_send(data).is_err() {
            self.bytes.fetch_sub(n, Ordering::AcqRel);
            if !coalesce {
                self.failed.store(true, Ordering::Release);
            }
        }
    }
    pub fn response(&self, id: &str, result: crate::model::Result<Value>) {
        let value = match result {
            Ok(r) => json!({"v":1,"id":id,"result":r}),
            Err(e) => json!({"v":1,"id":id,"error":e}),
        };
        self.enqueue(&mut self.inner.lock().unwrap(), &value, false);
    }
    pub fn event(&self, name: &str, data: Value) {
        let mut w = self.inner.lock().unwrap();
        w.seq += 1;
        let seq = w.seq;
        self.enqueue(&mut w,&json!({"v":1,"event":name,"seq":seq,"time_ms":self.start.elapsed().as_millis() as u64,"data":data}),name=="stats" || name=="log");
    }
    pub async fn drain(&self) {
        for _ in 0..50 {
            if self.bytes.load(Ordering::Acquire) == 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }
}
pub fn input() -> async_mpsc::Receiver<io::Result<Value>> {
    let (tx, rx) = async_mpsc::channel(128);
    std::thread::spawn(move || {
        let stdin = io::stdin();
        let mut r = stdin.lock();
        loop {
            match read_record(&mut r) {
                Ok(Some(v)) => {
                    if tx.blocking_send(Ok(v)).is_err() {
                        break;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    let _ = tx.blocking_send(Err(e));
                    break;
                }
            }
        }
    });
    rx
}
