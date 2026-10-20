//! Windows anonymous-pipe Chromium launcher. Independent of the synthetic video source.
use crate::browser_wire::{self as wire, *};
use crate::window_wire as ww;
use anyhow::{Context, Result, anyhow, bail, ensure};
use std::{
    collections::{BTreeSet, VecDeque},
    fs::File,
    io::Write,
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
        System::{IO::CancelSynchronousIo, JobObjects::*, Pipes::CreatePipe, Threading::*},
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
    job: OwnedHandle,
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
        let job = adopt(unsafe { CreateJobObjectW(None, PCWSTR::null())? });
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        unsafe {
            SetInformationJobObject(
                handle(&job),
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                size_of_val(&limits) as u32,
            )?;
        }
        // Vec<usize> guarantees attribute-list pointer alignment. Windows returns its byte size.
        let mut bytes = 0;
        let _ = unsafe { InitializeProcThreadAttributeList(None, 2, None, &mut bytes) };
        ensure!(bytes > 0, "No process attribute list size");
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        unsafe {
            InitializeProcThreadAttributeList(Some(list), 2, None, &mut bytes)?;
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
        // Atomic job assignment closes the parent-death race between
        // CreateProcess and a subsequent AssignProcessToJobObject call.
        let jobs = [handle(&job)];
        unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_JOB_LIST as usize,
                Some(jobs.as_ptr().cast()),
                size_of_val(&jobs),
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
            // CPU-rasterize Views tiles to avoid cold browser-UI shader
            // compilation blocking popup capture on Intel. Page rasterization,
            // hardware compositing, WebGL and DXGI capture remain enabled.
            if headless {
                "--headless=new --disable-features=UiGpuRasterization"
            } else {
                ""
            },
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
            text.push_str(
                " --enable-logging --vmodule=prts_controller=1,prts_capture=1,aura_input_barrier=1 --log-file=\"",
            );
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
                EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED,
                None,
                PCWSTR::null(),
                &startup.StartupInfo,
                &mut process,
            )?;
        }
        // CreateProcess grants full process access, including PROCESS_DUP_HANDLE and lifecycle rights.
        let process_handle = adopt(process.hProcess);
        let thread_handle = adopt(process.hThread);
        if unsafe { ResumeThread(handle(&thread_handle)) } == u32::MAX {
            unsafe {
                let _ = TerminateProcess(handle(&process_handle), 1);
            }
            bail!("Cannot resume contained Chromium");
        }
        drop(thread_handle);
        drop((cmd_child, evt_child, frame_child));
        Ok(Self {
            command: command.into(),
            event: event.into(),
            frame: frame.into(),
            job,
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
        self.wait_startup_cancellable(None)
    }
    fn wait_startup_cancellable(&self, cancel: Option<&AtomicBool>) -> Result<()> {
        let started = Instant::now();
        let timeout = startup_timeout()?;
        let result = loop {
            if cancel.is_some_and(|c| c.load(Ordering::Acquire)) {
                self.terminate();
                bail!("Browser startup cancelled");
            }
            let status = unsafe { WaitForInputIdle(handle(&self.process), 100) };
            if status != 258 || started.elapsed() >= timeout {
                break status;
            }
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
// Temporary single-selected-window adapter for the existing M4 media/control
// plane. IDs and frames are windows in v2; tab metadata remains in Window.
// Native v2 frontend/per-window workers are implemented in M5 step 6.
fn project_window(w: &ww::Window, selected: bool) -> Result<Vec<u8>> {
    let mut p = w.id.to_le_bytes().to_vec();
    p.push(w.kind.min(2));
    p.extend(w.opener.to_le_bytes());
    let active = w.tabs.iter().find(|t| t.id == w.active_tab);
    wire::string(active.map_or("", |t| t.url.as_str()), &mut p)?;
    wire::string(active.map_or("", |t| t.title.as_str()), &mut p)?;
    p.push(active.map_or(0, |t| t.load));
    w.size.encode(&mut p)?;
    p.extend(w.generation.to_le_bytes());
    p.push(selected as u8);
    Ok(p)
}
struct Session {
    major: u16,
    selected_window: u64,
    windows: std::collections::BTreeMap<u64, ww::Window>,
    scenes: std::collections::BTreeMap<u64, ww::Scene>,
    scene_frames: crate::scene_frames::Scenes,
    job: OwnedHandle,
    process: OwnedHandle,
    frames: crate::browser_frames::Resources,
    pid: u32,
    next_id: u64,
    outgoing: Option<SyncSender<Vec<u8>>>,
    command_bytes: Arc<AtomicUsize>,
    features: u32,
    gpu_import_failed: BTreeSet<u64>,
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
    fn start(mut raw: RawBrowser, selected_major: u16) -> Result<Self> {
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
                let mut major = if is_frame { selected_major } else { 1 };
                loop {
                    match Record::read_version(&mut file, major) {
                        Ok(record) => {
                            if !is_frame && record.kind == HELLO_ACK {
                                major = selected_major;
                            }
                            if is_frame && !matches!(record.kind, 0x310 | 0x311) {
                                let _ = tx.try_send(Incoming::Failure(
                                    "Invalid frame pipe direction".into(),
                                ));
                                break;
                            }
                            if !is_frame && matches!(record.kind, 0x310 | 0x311) {
                                let _ =
                                    tx.try_send(Incoming::Failure("Frame on event pipe".into()));
                                break;
                            }
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
                            let message = if is_frame {
                                Incoming::FrameClosed(format!("Frame pipe: {e}"))
                            } else {
                                Incoming::Failure(format!("Event pipe: {e}"))
                            };
                            let _ = tx.try_send(message);
                            break;
                        }
                    }
                }
            }));
        }
        let (outgoing, writes) = mpsc::sync_channel::<Vec<u8>>(1024);
        let command_bytes = Arc::new(AtomicUsize::new(0));
        let command_budget = command_bytes.clone();
        threads.push(thread::spawn(move || {
            while let Ok(bytes) = writes.recv() {
                if let Err(e) = command.write_all(&bytes) {
                    let _ = tx.try_send(Incoming::Failure(format!("Command pipe: {e}")));
                    break;
                }
                command_budget.fetch_sub(bytes.len(), Ordering::AcqRel);
            }
        }));
        raw.pid = 0; // Disarm raw launch RAII only after all resources have transferred.
        let session = Self {
            major: 1,
            selected_window: 0,
            windows: Default::default(),
            scenes: Default::default(),
            scene_frames: Default::default(),
            job: raw.job.try_clone()?,
            process,
            frames: Default::default(),
            pid: unsafe { GetProcessId(handle(&raw.process)) },
            next_id: 1,
            outgoing: Some(outgoing),
            command_bytes,
            features: 0,
            gpu_import_failed: BTreeSet::new(),
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
    fn select_window(&mut self, id: u64) -> Result<()> {
        let previous = std::mem::replace(&mut self.selected_window, id);
        if previous != id
            && let Some(old) = self.windows.get(&previous)
        {
            let record = Record {
                kind: CHANGED,
                id: 0,
                payload: project_window(old, false)?,
            };
            self.stash(record)?;
        }
        Ok(())
    }
    fn stash(&mut self, mut record: Record) -> Result<()> {
        if self.major == 2 {
            match record.kind {
                ww::WINDOW_CAPS => {
                    let mut d = Decoder::new(&record.payload);
                    for _ in 0..4 {
                        d.u32()?;
                    }
                    d.u64()?;
                    d.u64()?;
                    d.u32()?;
                    d.u32()?;
                    d.finish()?;
                    return Ok(());
                }
                ww::WINDOW_SCENE => {
                    let scene = ww::Scene::decode(&record.payload)?;
                    if let Some(previous) = self.scenes.get(&scene.window) {
                        ensure!(scene.revision > previous.revision, "Stale window scene");
                    }
                    let window = self
                        .windows
                        .get(&scene.window)
                        .context("Scene before window")?;
                    ensure!(
                        window.generation == scene.generation,
                        "Scene/window generation mismatch"
                    );
                    for layer in &scene.layers {
                        ensure!(
                            layer.source != window.id
                                && !window.tabs.iter().any(|t| t.id == layer.source),
                            "Source aliases window/tab identity"
                        );
                        ensure!(
                            !self
                                .scenes
                                .values()
                                .filter(|s| s.window != scene.window)
                                .any(|s| s.layers.iter().any(|l| l.source == layer.source)),
                            "Source crossed window owners"
                        );
                        if layer.role == 1 {
                            ensure!(
                                layer.parent == 0
                                    && layer.visible
                                    && layer.format == 1
                                    && layer.rect.x == 0.
                                    && layer.rect.y == 0.
                                    && layer.rect.width == f64::from(window.size.width)
                                    && layer.rect.height == f64::from(window.size.height)
                                    && layer.dpr == window.size.dpr,
                                "Incompatible base scene geometry"
                            );
                        }
                    }
                    self.scene_frames.update(scene.clone())?;
                    self.scenes.insert(scene.window, scene);
                    return Ok(());
                }
                ww::WINDOW_CREATED | ww::WINDOW_CHANGED => {
                    let window = ww::Window::decode(&record.payload)?;
                    if record.kind == ww::WINDOW_CREATED
                        && (self.selected_window == 0 || window.input_active)
                    {
                        self.select_window(window.id)?;
                    }
                    record.payload = project_window(&window, self.selected_window == window.id)?;
                    self.windows.insert(window.id, window);
                    record.kind = if record.kind == ww::WINDOW_CREATED {
                        CREATED
                    } else {
                        CHANGED
                    };
                }
                ww::WINDOW_CLOSED => {
                    let mut d = Decoder::new(&record.payload);
                    let id = d.u64()?;
                    d.string()?;
                    d.finish()?;
                    self.windows.remove(&id);
                    self.scenes.remove(&id);
                    self.scene_frames.remove(id);
                    if self.selected_window == id {
                        let next = self.windows.keys().next().copied().unwrap_or(0);
                        self.select_window(next)?;
                        if let Some(w) = self.windows.get(&next) {
                            self.stash(Record {
                                kind: CHANGED,
                                id: 0,
                                payload: project_window(w, true)?,
                            })?;
                        }
                    }
                    record.kind = CLOSED;
                }
                ww::WINDOW_CURSOR => record.kind = CURSOR_CHANGED,
                _ => {}
            }
        }
        if matches!(record.kind, 0x300 | 0x301 | 0x310 | 0x311) {
            if self.major == 2 && record.kind == 0x300 {
                let mut d = Decoder::new(&record.payload);
                let source = d.u64()?;
                let generation = d.u64()?;
                ensure!(
                    self.scenes.values().any(|s| s
                        .layers
                        .iter()
                        .any(|l| l.source == source && l.generation == generation)),
                    "Registration for unknown/stale source"
                );
            }
            let reply = match self.frames.process(&record, handle(&self.process)) {
                Ok(reply) => reply,
                Err(e) if record.kind == 0x300 && record.payload.get(24) == Some(&1) => {
                    let target = u64::from_le_bytes(record.payload[..8].try_into().unwrap());
                    if self.gpu_import_failed.insert(target) {
                        eprintln!(
                            "prtsd: GPU import failed target={target}; requesting CPU fallback: {e:#}"
                        );
                    }
                    let mut p = 9u32.to_le_bytes().to_vec();
                    let message = b"GPU texture import failed; use CPU sections";
                    p.extend((message.len() as u32).to_le_bytes());
                    p.extend(message);
                    p.push(0);
                    Record {
                        kind: 3,
                        id: record.id,
                        payload: p,
                    }
                }
                Err(e) => return self.fail(format!("Frame resource error: {e:#}")),
            };
            self.enqueue(reply.encode_version(self.major)?)?;
            if self.major == 2 {
                while let Some(frame) = self.frames.take_frame() {
                    self.scene_frames.push(frame)?;
                }
            }
            return Ok(());
        }
        let size = record.payload.len() + 24;
        let event = match record.event() {
            Ok(e) => e,
            Err(e) => return self.fail(e.to_string()),
        };
        if matches!(
            event,
            Event::Cursor {
                cursor: wire::Cursor { shape: 8, .. },
                ..
            }
        ) && self.features & 2 == 0
        {
            return self.fail("Unnegotiated custom cursor");
        }
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
    fn enqueue(&mut self, bytes: Vec<u8>) -> Result<()> {
        if self.command_bytes.fetch_add(bytes.len(), Ordering::AcqRel) + bytes.len() > QUEUE_BYTES {
            return self.fail("Command byte queue overflow");
        }
        if self
            .outgoing
            .as_ref()
            .context("Writer closed")?
            .try_send(bytes)
            .is_err()
        {
            return self.fail("Command queue overflow/disconnect");
        }
        Ok(())
    }
    fn request(&mut self, kind: u16, payload: Vec<u8>, expected: u16) -> Result<Record> {
        if let Some(e) = &self.failed {
            bail!("Browser failed: {e}");
        }
        ensure!(!self.shutdown, "Browser shut down");
        let id = self.next_id;
        self.next_id = self.next_id.checked_add(2).context("Request ID overflow")?;
        let mut wire_kind = kind;
        let mut payload = payload;
        if self.major == 2 {
            wire_kind = match kind {
                CREATE => ww::WINDOW_CREATE,
                CLOSE => ww::WINDOW_CLOSE,
                LIST => ww::WINDOW_LIST,
                RESIZE => ww::WINDOW_RESIZE,
                ACTIVATE => ww::WINDOW_ACTIVATE,
                0x321 => ww::WINDOW_FRAME_CONFIG,
                0x322 => ww::WINDOW_FRAME_REFRESH,
                NAVIGATE => {
                    ensure!(payload.len() >= 8, "Truncated window navigation");
                    let id = u64::from_le_bytes(payload[..8].try_into()?);
                    let tab = self
                        .windows
                        .get(&id)
                        .context("Unknown navigation window")?
                        .active_tab;
                    payload[..8].copy_from_slice(&tab.to_le_bytes());
                    ww::TAB_NAVIGATE
                }
                _ => kind,
            };
        }
        let bytes = Record {
            kind: wire_kind,
            id,
            payload,
        }
        .encode_version(self.major)?;
        self.enqueue(bytes)?;
        self.last_request = Instant::now();
        let deadline = Instant::now() + DEADLINE;
        loop {
            let mut record = self.incoming(
                deadline.saturating_duration_since(Instant::now()),
                kind == SHUTDOWN,
            )?;
            if record.id == 0 || record.id % 2 == 0 {
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
            if self.major == 2 && record.kind == ww::WINDOW_RESULT {
                let w = ww::Window::decode(&record.payload)?;
                if kind == ACTIVATE {
                    self.select_window(w.id)?;
                    self.stash(Record {
                        kind: CHANGED,
                        id: 0,
                        payload: project_window(&w, true)?,
                    })?;
                }
                record.payload = project_window(&w, self.selected_window == w.id)?;
                self.windows.insert(w.id, w);
                record.kind = TARGET_RESULT;
            } else if self.major == 2 && record.kind == ww::WINDOW_LIST_RESULT {
                let windows = ww::Window::list(&record.payload)?;
                record.payload = (windows.len() as u32).to_le_bytes().to_vec();
                for w in windows {
                    record
                        .payload
                        .extend(project_window(&w, self.selected_window == w.id)?);
                    self.windows.insert(w.id, w);
                }
                record.kind = LIST_RESULT;
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
        let _ = unsafe { TerminateJobObject(handle(&self.job), 0) };
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
    pub fn launch_cancellable(executable: impl AsRef<Path>, cancel: &AtomicBool) -> Result<Self> {
        Self::connect_internal(RawBrowser::launch(executable.as_ref())?, Some(cancel))
    }
    pub fn connect(raw: RawBrowser) -> Result<Self> {
        Self::connect_internal(raw, None)
    }
    fn connect_internal(raw: RawBrowser, cancel: Option<&AtomicBool>) -> Result<Self> {
        raw.wait_startup_cancellable(cancel)?;
        let major = match std::env::var("PRTS_BROWSER_PROTOCOL").as_deref() {
            Ok("2") => 2,
            Ok("1") | Err(_) => 1,
            _ => bail!("PRTS_BROWSER_PROTOCOL must be 1|2"),
        };
        let mut session = Session::start(raw, major)?;
        let mut hello = Vec::new();
        for v in [major, 0, major, 0] {
            hello.extend(v.to_le_bytes());
        }
        hello.extend(std::process::id().to_le_bytes());
        let mode = std::env::var("PRTS_FRAME_MODE").unwrap_or_else(|_| "auto".into());
        ensure!(
            matches!(mode.as_str(), "auto" | "gpu" | "cpu"),
            "PRTS_FRAME_MODE must be auto|gpu|cpu"
        );
        let luid = if mode == "cpu" {
            0
        } else {
            crate::d3d::shared()
                .map(|d| d.lock().unwrap().luid)
                .unwrap_or(0)
        };
        hello.extend(luid.to_le_bytes());
        hello.extend((if luid == 0 { 2u32 } else { 3u32 }).to_le_bytes());
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
            d.u16()? == major && d.u16()? == 0,
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
                && (hello.frame_modes == 2
                    || (hello.frame_modes == 3 && hello.adapter_luid == luid && luid != 0))
                && (1..=3).contains(&hello.max_inflight)
                && hello.max_width > 0
                && hello.max_height > 0
                && hello.max_pixels > 0
                && hello.features & !63 == 0
                && (major == 1 || hello.features & ww::WHOLE_WINDOW != 0),
            "Invalid HELLO_ACK capabilities"
        );
        eprintln!(
            "prtsd: frame negotiation modes={} adapter_luid={:#018x}",
            hello.frame_modes, hello.adapter_luid
        );
        session
            .frames
            .set_gpu_adapter(if hello.frame_modes & 1 != 0 {
                hello.adapter_luid
            } else {
                0
            });
        session.features = hello.features;
        session
            .frames
            .set_layer_alpha(major == 2 && hello.features & ww::PREMULTIPLIED_BGRA != 0);
        session.major = major;
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
    pub fn configure_frames(&self, target: u64, fps: u32) -> Result<Target> {
        self.configure_frames_with_limit(target, fps, self.hello.max_inflight.min(2))
    }
    pub fn configure_frames_with_limit(
        &self,
        target: u64,
        fps: u32,
        max_inflight: u32,
    ) -> Result<Target> {
        ensure!(
            fps == 0 || (max_inflight > 0 && max_inflight <= self.hello.max_inflight.min(2)),
            "Invalid frame credit limit"
        );
        let mut p = target.to_le_bytes().to_vec();
        p.push(if fps == 0 {
            0
        } else if self.hello.frame_modes & 1 != 0 {
            1
        } else {
            2
        });
        p.extend((if fps == 0 { 0u32 } else { max_inflight }).to_le_bytes());
        p.extend(fps.to_le_bytes());
        self.target_request(0x321, p)
    }
    pub fn refresh_frame(&self, target: u64) -> Result<()> {
        self.empty(0x322, target.to_le_bytes().to_vec())
    }
    pub fn poll_frame(&self) -> Result<Option<crate::browser_frames::CpuFrame>> {
        let mut s = self
            .session
            .lock()
            .map_err(|_| anyhow!("Session poisoned"))?;
        if s.overflow.load(Ordering::Acquire) {
            return s.fail("Browser input queue overflow");
        }
        if let Some(e) = &s.failed {
            bail!("{e}")
        }
        for _ in 0..1024 {
            match s.incoming.try_recv() {
                Ok(Incoming::Record(r, size)) => {
                    s.queued_bytes.fetch_sub(size, Ordering::AcqRel);
                    s.queued_messages.fetch_sub(1, Ordering::AcqRel);
                    s.stash(r)?;
                }
                Ok(Incoming::Failure(e) | Incoming::FrameClosed(e)) => return s.fail(e),
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => return s.fail("Browser readers stopped"),
            }
        }
        if s.major == 2 {
            return s.scene_frames.take_frame();
        }
        Ok(s.frames.take_frame())
    }
    pub fn take_frame(&self) -> Option<crate::browser_frames::CpuFrame> {
        self.poll_frame().ok().flatten()
    }
    pub fn frame_counts(&self) -> (u64, u64) {
        let s = self.session.lock().unwrap();
        (s.frames.received, s.frames.coalesced)
    }
    /// Acceptance diagnostics: CPU seconds for the contained browser tree,
    /// peak job committed bytes, and live imported section count.
    pub fn resource_usage(&self) -> Result<(f64, usize, usize)> {
        let s = self
            .session
            .lock()
            .map_err(|_| anyhow!("Session poisoned"))?;
        let mut cpu = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        let mut memory = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        unsafe {
            QueryInformationJobObject(
                Some(handle(&s.job)),
                JobObjectBasicAccountingInformation,
                &mut cpu as *mut _ as *mut _,
                size_of_val(&cpu) as u32,
                None,
            )?;
            QueryInformationJobObject(
                Some(handle(&s.job)),
                JobObjectExtendedLimitInformation,
                &mut memory as *mut _ as *mut _,
                size_of_val(&memory) as u32,
                None,
            )?;
        }
        Ok((
            (cpu.TotalUserTime + cpu.TotalKernelTime) as f64 / 10_000_000.,
            memory.PeakJobMemoryUsed,
            s.frames.resource_count(),
        ))
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
    /// Ordered, bounded notification; failures are delivered as FAULT events.
    pub fn input(&self, header: &wire::InputHeader, input: &wire::Input) -> Result<()> {
        let mut s = self
            .session
            .lock()
            .map_err(|_| anyhow!("Session poisoned"))?;
        ensure!(s.failed.is_none() && !s.shutdown, "Browser unavailable");
        let bytes = input.record(header)?.encode_version(s.major)?;
        s.enqueue(bytes)
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
                        if record.id != 0 && record.id % 2 != 0 {
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
