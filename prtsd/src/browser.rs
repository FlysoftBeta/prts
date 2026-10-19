//! Windows anonymous-pipe Chromium launcher. Independent of the synthetic video source.
use crate::browser_wire::{self as wire, *};
use anyhow::{Context, Result, anyhow, bail, ensure};
use std::{
    collections::VecDeque,
    fs::File,
    io::{Read, Write},
    mem::size_of,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0},
        Security::SECURITY_ATTRIBUTES,
        System::{IO::CancelSynchronousIo, Pipes::CreatePipe, Threading::*},
    },
    core::{PCWSTR, PWSTR},
};

const DEADLINE: Duration = Duration::from_secs(5);
const QUEUE_BYTES: usize = 8 * 1024 * 1024;
pub const DEFAULT_CHROME: &str = r"C:\src\chromium\src\out\Default\chrome.exe";
pub fn startup_timeout() -> Result<Duration> {
    let seconds = std::env::var("PRTS_BROWSER_STARTUP_TIMEOUT_SECS")
        .ok()
        .map(|v| v.parse::<u64>())
        .transpose()?
        .unwrap_or(300);
    ensure!(
        (5..=600).contains(&seconds),
        "Startup timeout must be5..600 seconds"
    );
    Ok(Duration::from_secs(seconds))
}

fn handle(h: &impl AsRawHandle) -> HANDLE {
    HANDLE(h.as_raw_handle())
}
fn adopt(h: HANDLE) -> OwnedHandle {
    unsafe { OwnedHandle::from_raw_handle(h.0) }
}
fn pipe(child_reads: bool) -> Result<(OwnedHandle, OwnedHandle)> {
    let mut read = HANDLE::default();
    let mut write = HANDLE::default();
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        bInheritHandle: true.into(),
        ..Default::default()
    };
    unsafe {
        CreatePipe(&mut read, &mut write, Some(&attributes), 65536)?;
    }
    let read = adopt(read);
    let write = adopt(write);
    let (parent, child) = if child_reads {
        (write, read)
    } else {
        (read, write)
    };
    unsafe {
        SetHandleInformation(handle(&parent), HANDLE_FLAG_INHERIT.0, Default::default())?;
    }
    Ok((parent, child))
}

/// Raw launch is public only to support malformed/partial handshake acceptance tests.
pub struct RawBrowser {
    pub command: File,
    pub event: File,
    pub frame: File,
    process: OwnedHandle,
    pub pid: u32,
}
impl RawBrowser {
    pub fn launch(executable: &Path) -> Result<Self> {
        Self::launch_with_switches(executable, &[])
    }
    /// Extra switches are for standalone qualification, not tenant input.
    pub fn launch_with_switches(executable: &Path, extra_switches: &[&str]) -> Result<Self> {
        Self::launch_configuration(executable, extra_switches, true)
    }
    /// Omitting headless is exclusively a negative bootstrap test.
    pub fn launch_configuration(
        executable: &Path,
        extra_switches: &[&str],
        headless: bool,
    ) -> Result<Self> {
        let (command, cmd_child) = pipe(true)?;
        let (event, evt_child) = pipe(false)?;
        let (frame, frame_child) = pipe(false)?;
        let children = [handle(&cmd_child), handle(&evt_child), handle(&frame_child)];
        // Vec<usize> guarantees attribute-list pointer alignment. Windows returns its byte size.
        let mut bytes = 0;
        let _ = unsafe { InitializeProcThreadAttributeList(None, 1, None, &mut bytes) };
        ensure!(bytes > 0, "No process attribute list size");
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        unsafe {
            InitializeProcThreadAttributeList(Some(list), 1, None, &mut bytes)?;
        }
        struct Attributes(LPPROC_THREAD_ATTRIBUTE_LIST);
        impl Drop for Attributes {
            fn drop(&mut self) {
                unsafe {
                    DeleteProcThreadAttributeList(self.0);
                }
            }
        }
        let _attributes = Attributes(list);
        unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(children.as_ptr().cast()),
                size_of_val(&children),
                None,
                None,
            )?;
        }
        let executable_text = executable.to_str().context("Chrome path is not UTF-8")?;
        ensure!(
            !executable_text.contains('"') && !executable_text.contains('\0'),
            "Invalid executable path"
        );
        let app: Vec<u16> = executable_text.encode_utf16().chain(Some(0)).collect();
        let mut text = format!(
            "\"{}\" {} --no-first-run --no-default-browser-check --prts-pipe=cmd:0x{:X},evt:0x{:X},frame:0x{:X}",
            executable_text,
            if headless { "--headless=new" } else { "" },
            children[0].0 as usize,
            children[1].0 as usize,
            children[2].0 as usize
        );
        for switch in extra_switches {
            ensure!(
                switch.starts_with("--")
                    && !switch
                        .chars()
                        .any(|c| c.is_whitespace() || c == '"' || c == '\0'),
                "Expected one literal switch"
            );
            text.push(' ');
            text.push_str(switch);
        }
        if let Ok(log) = std::env::var("PRTS_BROWSER_LOG") {
            ensure!(
                !log.contains('"') && !log.contains('\0'),
                "Invalid browser log path"
            );
            text.push_str(" --enable-logging --vmodule=prts_controller=1 --log-file=\"");
            text.push_str(&log);
            text.push('"');
        }
        let mut command_line: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
        let mut startup = STARTUPINFOEXW::default();
        startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
        startup.lpAttributeList = list;
        let mut process = PROCESS_INFORMATION::default();
        unsafe {
            CreateProcessW(
                PCWSTR(app.as_ptr()),
                Some(PWSTR(command_line.as_mut_ptr())),
                None,
                None,
                true,
                EXTENDED_STARTUPINFO_PRESENT,
                None,
                PCWSTR::null(),
                &startup.StartupInfo,
                &mut process,
            )?;
        }
        // CreateProcess grants full process access, including PROCESS_DUP_HANDLE and lifecycle rights.
        let process_handle = adopt(process.hProcess);
        drop(adopt(process.hThread));
        drop((cmd_child, evt_child, frame_child));
        Ok(Self {
            command: command.into(),
            event: event.into(),
            frame: frame.into(),
            process: process_handle,
            pid: process.dwProcessId,
        })
    }
    pub fn wait(&self, timeout: Duration) -> bool {
        unsafe {
            WaitForSingleObject(
                handle(&self.process),
                timeout.as_millis().min(u32::MAX as u128) as u32,
            ) == WAIT_OBJECT_0
        }
    }
    pub fn terminate(&self) {
        unsafe {
            let _ = TerminateProcess(handle(&self.process), 1);
        }
    }
    pub fn process_handle(&self) -> HANDLE {
        handle(&self.process)
    }
    /// Component builds may spend substantial time in the Windows DLL loader.
    /// This startup deadline is separate from the five-second HELLO_ACK deadline.
    pub fn wait_startup(&self) -> Result<()> {
        let started = Instant::now();
        let result = unsafe {
            WaitForInputIdle(handle(&self.process), startup_timeout()?.as_millis() as u32)
        };
        ensure!(
            result == 0,
            "Browser startup readiness failed result={result},exit_code={},elapsed={:.2}s",
            self.exit_code()?,
            started.elapsed().as_secs_f64()
        );
        if std::env::var_os("PRTS_BROWSER_LOG").is_some() {
            eprintln!(
                "prtsd: browser startup input-idle after {:.2}s pid={}",
                started.elapsed().as_secs_f64(),
                self.pid
            );
        }
        Ok(())
    }
    pub fn exit_code(&self) -> Result<u32> {
        let mut code = 0;
        unsafe {
            GetExitCodeProcess(handle(&self.process), &mut code)?;
        }
        Ok(code)
    }
}
impl Drop for RawBrowser {
    fn drop(&mut self) {
        if self.pid != 0 && !self.wait(Duration::ZERO) {
            self.terminate();
            let _ = self.wait(DEADLINE);
        }
    }
}

#[derive(Debug, Clone)]
pub struct HelloAck {
    pub browser_pid: u32,
    pub adapter_luid: u64,
    pub frame_modes: u32,
    pub max_inflight: u32,
    pub max_width: u32,
    pub max_height: u32,
    pub max_pixels: u32,
    pub features: u32,
}

enum Incoming {
    Record(Record, usize),
    Failure(String),
    FrameClosed(String),
}
struct Session {
    process: OwnedHandle,
    pid: u32,
    next_id: u64,
    outgoing: Option<SyncSender<Vec<u8>>>,
    incoming: Receiver<Incoming>,
    queued_bytes: Arc<AtomicUsize>,
    queued_messages: Arc<AtomicUsize>,
    overflow: Arc<AtomicBool>,
    threads: Vec<JoinHandle<()>>,
    events: VecDeque<(Event, usize)>,
    event_bytes: usize,
    last_request: Instant,
    failed: Option<String>,
    shutdown: bool,
    // Ordered transcript permits acceptance checks to prove event-before-response behavior.
    transcript: VecDeque<(u16, u64)>,
}
impl Session {
    fn start(mut raw: RawBrowser) -> Result<Self> {
        let (tx, incoming) = mpsc::sync_channel(1024);
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        let queued_messages = Arc::new(AtomicUsize::new(0));
        let overflow = Arc::new(AtomicBool::new(false));
        let mut threads = Vec::new();
        // Transfer cloned local pipe ownership; RawBrowser is disarmed below.
        let event = raw.event.try_clone()?;
        let frame = raw.frame.try_clone()?;
        let mut command = raw.command.try_clone()?;
        let process = raw.process.try_clone()?;
        for (mut file, is_frame) in [(event, false), (frame, true)] {
            let tx = tx.clone();
            let budget = queued_bytes.clone();
            let count = queued_messages.clone();
            let overflow = overflow.clone();
            threads.push(thread::spawn(move || {
                if is_frame {
                    let mut byte = [0u8; 1];
                    let message = match file.read(&mut byte) {
                        Ok(0) => Incoming::FrameClosed("Frame pipe EOF".into()),
                        Ok(_) => Incoming::Failure("Unexpected frame bytes in M1".into()),
                        Err(e) => Incoming::FrameClosed(format!("Frame pipe: {e}")),
                    };
                    let _ = tx.try_send(message);
                    return;
                }
                loop {
                    match Record::read(&mut file) {
                        Ok(record) => {
                            let size = record.payload.len() + 24;
                            if budget.fetch_add(size, Ordering::AcqRel) + size > QUEUE_BYTES
                                || count.fetch_add(1, Ordering::AcqRel) >= 1024
                            {
                                overflow.store(true, Ordering::Release);
                                let _ = tx.try_send(Incoming::Failure(
                                    "Event byte queue overflow".into(),
                                ));
                                break;
                            }
                            if tx.try_send(Incoming::Record(record, size)).is_err() {
                                overflow.store(true, Ordering::Release);
                                let _ = tx.try_send(Incoming::Failure(
                                    "Event message queue overflow".into(),
                                ));
                                break;
                            }
                        }
                        Err(e) => {
                            let _ = tx.try_send(Incoming::Failure(format!("Event pipe: {e}")));
                            break;
                        }
                    }
                }
            }));
        }
        let (outgoing, writes) = mpsc::sync_channel::<Vec<u8>>(1024);
        threads.push(thread::spawn(move || {
            while let Ok(bytes) = writes.recv() {
                if let Err(e) = command.write_all(&bytes) {
                    let _ = tx.try_send(Incoming::Failure(format!("Command pipe: {e}")));
                    break;
                }
            }
        }));
        raw.pid = 0; // Disarm raw launch RAII only after all resources have transferred.
        let session = Self {
            process,
            pid: unsafe { GetProcessId(handle(&raw.process)) },
            next_id: 1,
            outgoing: Some(outgoing),
            incoming,
            queued_bytes,
            queued_messages,
            overflow,
            threads,
            events: VecDeque::new(),
            event_bytes: 0,
            last_request: Instant::now(),
            failed: None,
            shutdown: false,
            transcript: VecDeque::new(),
        };
        // RawBrowser::drop skips termination for a successfully transferred launch.
        Ok(session)
    }
    fn fail<T>(&mut self, message: impl Into<String>) -> Result<T> {
        let mut code = 0;
        let _ = unsafe { GetExitCodeProcess(handle(&self.process), &mut code) };
        let message = format!(
            "{} (browser_pid={}, exit_code={code}, alive={})",
            message.into(),
            self.pid,
            unsafe { WaitForSingleObject(handle(&self.process), 0) } != WAIT_OBJECT_0
        );
        self.failed = Some(message.clone());
        unsafe {
            let _ = TerminateProcess(handle(&self.process), 1);
        }
        bail!(message)
    }
    fn incoming(&mut self, timeout: Duration, shutting_down: bool) -> Result<Record> {
        let deadline = Instant::now() + timeout;
        loop {
            if self.overflow.load(Ordering::Acquire) {
                return self.fail("Control queue overflow");
            }
            return match self
                .incoming
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            {
                Ok(Incoming::Record(record, size)) => {
                    self.queued_bytes.fetch_sub(size, Ordering::AcqRel);
                    self.queued_messages.fetch_sub(1, Ordering::AcqRel);
                    if self.transcript.len() == 4096 {
                        self.transcript.pop_front();
                    }
                    self.transcript.push_back((record.kind, record.id));
                    Ok(record)
                }
                Ok(Incoming::Failure(e)) => self.fail(e),
                Ok(Incoming::FrameClosed(e)) => {
                    if shutting_down {
                        continue;
                    } else {
                        self.fail(e)
                    }
                }
                Err(e) => self.fail(format!("Browser receive deadline/disconnect: {e}")),
            };
        }
    }
    fn stash(&mut self, record: Record) -> Result<()> {
        let size = record.payload.len() + 24;
        let event = match record.event() {
            Ok(e) => e,
            Err(e) => return self.fail(e.to_string()),
        };
        if let Event::Fault {
            fatal: true,
            ref message,
            ..
        } = event
        {
            return self.fail(format!("Browser fatal fault: {message}"));
        }
        if self.queued_messages.fetch_add(1, Ordering::AcqRel) >= 1024
            || self.queued_bytes.fetch_add(size, Ordering::AcqRel) + size > QUEUE_BYTES
        {
            return self.fail("Unconsumed target event queue overflow");
        }
        self.event_bytes += size;
        self.events.push_back((event, size));
        Ok(())
    }
    fn request(&mut self, kind: u16, payload: Vec<u8>, expected: u16) -> Result<Record> {
        if let Some(e) = &self.failed {
            bail!("Browser failed: {e}");
        }
        ensure!(!self.shutdown, "Browser shut down");
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(2).context("Request ID overflow")?;
        let bytes = Record { kind, id, payload }.encode()?;
        if self
            .outgoing
            .as_ref()
            .context("Writer closed")?
            .try_send(bytes)
            .is_err()
        {
            return self.fail("Command queue overflow/disconnect");
        }
        self.last_request = Instant::now();
        let deadline = Instant::now() + DEADLINE;
        loop {
            let record = self.incoming(
                deadline.saturating_duration_since(Instant::now()),
                kind == SHUTDOWN,
            )?;
            if record.id == 0 {
                if kind == HELLO {
                    return self.fail("Event before HELLO_ACK");
                }
                self.stash(record)?;
                continue;
            }
            if record.id != id || record.id % 2 == 0 {
                return self.fail("Unexpected response request ID");
            }
            if record.kind == ERROR {
                let decoded = (|| -> Result<_> {
                    let mut d = Decoder::new(&record.payload);
                    let code = d.u32()?;
                    ensure!((1..=10).contains(&code), "Invalid error code");
                    let message = d.string()?;
                    let retryable = d.boolean()?;
                    d.finish()?;
                    Ok((code, message, retryable))
                })();
                let (code, message, retryable) = match decoded {
                    Ok(v) => v,
                    Err(e) => return self.fail(format!("Malformed ERROR: {e}")),
                };
                bail!("Browser error {code} (retryable={retryable}): {message}");
            }
            if record.kind != expected {
                return self.fail(format!("Unexpected response type {}", record.kind));
            }
            return Ok(record);
        }
    }
}
impl Drop for Session {
    fn drop(&mut self) {
        // Termination closes peer ends, then CancelSynchronousIo wakes blocked local workers.
        // No reader holds a reference to the session or to the browser process handle.
        if unsafe { WaitForSingleObject(handle(&self.process), 0) } != WAIT_OBJECT_0 {
            unsafe {
                let _ = TerminateProcess(handle(&self.process), 1);
            }
        }
        self.outgoing.take();
        for worker in &self.threads {
            unsafe {
                let _ = CancelSynchronousIo(HANDLE(worker.as_raw_handle()));
            }
        }
        for worker in self.threads.drain(..) {
            let _ = worker.join();
        }
        unsafe {
            let _ = WaitForSingleObject(handle(&self.process), 5000);
        }
    }
}

pub struct Browser {
    session: Arc<Mutex<Session>>,
    stop: Arc<AtomicBool>,
    heartbeat: Option<JoinHandle<()>>,
    pub hello: HelloAck,
}
impl Browser {
    pub fn launch(executable: impl AsRef<Path>) -> Result<Self> {
        Self::connect(RawBrowser::launch(executable.as_ref())?)
    }
    pub fn connect(raw: RawBrowser) -> Result<Self> {
        raw.wait_startup()?;
        let mut session = Session::start(raw)?;
        let mut hello = Vec::new();
        for v in [1u16, 0, 1, 0] {
            hello.extend(v.to_le_bytes());
        }
        hello.extend(std::process::id().to_le_bytes());
        hello.extend(0u64.to_le_bytes());
        hello.extend(2u32.to_le_bytes());
        hello.extend(3u32.to_le_bytes());
        let hello_sent = Instant::now();
        let response = session.request(HELLO, hello, HELLO_ACK)?;
        if std::env::var_os("PRTS_BROWSER_LOG").is_some() {
            eprintln!(
                "prtsd: HELLO_ACK received after{:.2}ms (protocol deadline5000ms)",
                hello_sent.elapsed().as_secs_f64() * 1000.
            );
        }
        let mut d = Decoder::new(&response.payload);
        ensure!(
            d.u16()? == 1 && d.u16()? == 0,
            "Unsupported selected version"
        );
        let hello = HelloAck {
            browser_pid: d.u32()?,
            adapter_luid: d.u64()?,
            frame_modes: d.u32()?,
            max_inflight: d.u32()?,
            max_width: d.u32()?,
            max_height: d.u32()?,
            max_pixels: d.u32()?,
            features: d.u32()?,
        };
        d.finish()?;
        ensure!(
            hello.browser_pid == session.pid
                && hello.frame_modes == 2
                && (1..=3).contains(&hello.max_inflight)
                && hello.max_width > 0
                && hello.max_height > 0
                && hello.max_pixels > 0
                && hello.features & !3 == 0,
            "Invalid HELLO_ACK capabilities"
        );
        let session = Arc::new(Mutex::new(session));
        let weak = Arc::downgrade(&session);
        let stop = Arc::new(AtomicBool::new(false));
        let thread_stop = stop.clone();
        let heartbeat = thread::spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                thread::park_timeout(Duration::from_secs(1));
                let Some(session) = weak.upgrade() else { break };
                let Ok(mut session) = session.lock() else {
                    break;
                };
                if session.shutdown || session.failed.is_some() {
                    break;
                }
                if session.last_request.elapsed() >= Duration::from_secs(5) {
                    let cookie = session.next_id;
                    let result = session
                        .request(PING, cookie.to_le_bytes().to_vec(), PONG)
                        .and_then(|r| {
                            ensure!(r.payload == cookie.to_le_bytes(), "PONG cookie mismatch");
                            Ok(())
                        });
                    if let Err(e) = result {
                        let _: Result<()> = session.fail(e.to_string());
                        break;
                    }
                }
            }
        });
        Ok(Self {
            session,
            stop,
            heartbeat: Some(heartbeat),
            hello,
        })
    }
    pub fn pid(&self) -> u32 {
        self.hello.browser_pid
    }
    fn request(&self, kind: u16, payload: Vec<u8>, expected: u16) -> Result<Record> {
        self.session
            .lock()
            .map_err(|_| anyhow!("Session poisoned"))?
            .request(kind, payload, expected)
    }
    pub fn create(&self, url: &str, size: Size, activate: bool) -> Result<Target> {
        let mut p = Vec::new();
        wire::string(url, &mut p)?;
        size.encode(&mut p)?;
        p.push(activate as u8);
        self.target_request(CREATE, p)
    }
    pub fn close(&self, target: u64) -> Result<()> {
        self.empty(CLOSE, target.to_le_bytes().to_vec())
    }
    pub fn navigate(&self, target: u64, url: &str) -> Result<()> {
        let mut p = target.to_le_bytes().to_vec();
        wire::string(url, &mut p)?;
        self.empty(NAVIGATE, p)
    }
    pub fn resize(&self, target: u64, size: Size) -> Result<Target> {
        let mut p = target.to_le_bytes().to_vec();
        size.encode(&mut p)?;
        self.target_request(RESIZE, p)
    }
    pub fn activate(&self, target: u64) -> Result<Target> {
        self.target_request(ACTIVATE, target.to_le_bytes().to_vec())
    }
    pub fn list(&self) -> Result<Vec<Target>> {
        let r = self.request(LIST, vec![], LIST_RESULT)?;
        self.decode(&r, |d| {
            let n = d.u32()?;
            ensure!(n <= 256, "Too many targets");
            let mut targets = Vec::new();
            let mut ids = std::collections::HashSet::new();
            for _ in 0..n {
                let target = d.target()?;
                ensure!(ids.insert(target.id), "Duplicate listed target ID");
                targets.push(target);
            }
            Ok(targets)
        })
    }
    fn decode<T>(
        &self,
        record: &Record,
        decode: impl FnOnce(&mut Decoder<'_>) -> Result<T>,
    ) -> Result<T> {
        let result: Result<T> = (|| {
            let mut d = Decoder::new(&record.payload);
            let value = decode(&mut d)?;
            d.finish()?;
            Ok(value)
        })();
        match result {
            Ok(value) => Ok(value),
            Err(e) => self
                .session
                .lock()
                .map_err(|_| anyhow!("Session poisoned"))?
                .fail(format!("Malformed response payload: {e}")),
        }
    }
    fn empty(&self, kind: u16, p: Vec<u8>) -> Result<()> {
        self.decode(&self.request(kind, p, OK)?, |_| Ok(()))
    }
    fn target_request(&self, kind: u16, p: Vec<u8>) -> Result<Target> {
        let r = self.request(kind, p, TARGET_RESULT)?;
        self.decode(&r, |d| d.target())
    }
    pub fn ping(&self, cookie: u64) -> Result<()> {
        let record = self.request(PING, cookie.to_le_bytes().to_vec(), PONG)?;
        self.decode(&record, |d| {
            ensure!(d.u64()? == cookie, "PONG cookie mismatch");
            Ok(())
        })
    }
    pub fn drain_events(&self) -> Vec<Event> {
        let mut s = self.session.lock().unwrap();
        s.queued_bytes.fetch_sub(s.event_bytes, Ordering::AcqRel);
        s.queued_messages
            .fetch_sub(s.events.len(), Ordering::AcqRel);
        s.event_bytes = 0;
        s.events.drain(..).map(|(event, _)| event).collect()
    }
    /// Wait for a lifecycle event without blocking the independent pipe readers.
    /// A quiet event stream returns None; request/EOF failures remain fatal.
    pub fn receive_event(&self, timeout: Duration) -> Result<Option<Event>> {
        let deadline = Instant::now() + timeout;
        loop {
            {
                let mut s = self
                    .session
                    .lock()
                    .map_err(|_| anyhow!("Session poisoned"))?;
                if s.overflow.load(Ordering::Acquire) {
                    return s.fail("Control queue overflow");
                }
                if let Some(message) = &s.failed {
                    bail!("Browser failed: {message}");
                }
                if let Some((event, size)) = s.events.pop_front() {
                    s.event_bytes -= size;
                    s.queued_bytes.fetch_sub(size, Ordering::AcqRel);
                    s.queued_messages.fetch_sub(1, Ordering::AcqRel);
                    return Ok(Some(event));
                }
                match s.incoming.try_recv() {
                    Ok(Incoming::Record(record, size)) => {
                        s.queued_bytes.fetch_sub(size, Ordering::AcqRel);
                        s.queued_messages.fetch_sub(1, Ordering::AcqRel);
                        if s.transcript.len() == 4096 {
                            s.transcript.pop_front();
                        }
                        s.transcript.push_back((record.kind, record.id));
                        if record.id != 0 {
                            return s.fail("Unexpected unsolicited response");
                        }
                        s.stash(record)?;
                        continue;
                    }
                    Ok(Incoming::Failure(message)) => return s.fail(message),
                    Ok(Incoming::FrameClosed(message)) => return s.fail(message),
                    Err(mpsc::TryRecvError::Disconnected) => {
                        return s.fail("Event queue disconnected");
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                }
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            thread::sleep(
                Duration::from_millis(5).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }
    pub fn transcript(&self) -> Vec<(u16, u64)> {
        self.session
            .lock()
            .unwrap()
            .transcript
            .iter()
            .copied()
            .collect()
    }
    pub fn shutdown(&self) -> Result<()> {
        self.empty(SHUTDOWN, vec![])?;
        let mut s = self.session.lock().unwrap();
        s.shutdown = true;
        ensure!(
            unsafe { WaitForSingleObject(handle(&s.process), 5000) } == WAIT_OBJECT_0,
            "Browser did not exit after OK"
        );
        Ok(())
    }
}
impl Drop for Browser {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.heartbeat.take() {
            h.thread().unpark();
            let _ = h.join();
        }
    }
}
