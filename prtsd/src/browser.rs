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
    job: Option<OwnedHandle>,
    process: OwnedHandle,
    state: Arc<crate::isolation::state::State>,
    desktop_helper: Option<Box<RawBrowser>>,
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
        Self::launch_isolated(
            executable,
            extra_switches,
            headless,
            &crate::isolation::Config::from_env()?,
        )
    }
    pub fn launch_isolated(
        executable: &Path,
        extra_switches: &[&str],
        headless: bool,
        isolation: &crate::isolation::Config,
    ) -> Result<Self> {
        Self::launch_internal(executable, extra_switches, headless, isolation, None)
    }
    fn launch_internal(
        executable: &Path,
        extra_switches: &[&str],
        headless: bool,
        isolation: &crate::isolation::Config,
        inherited_job: Option<&OwnedHandle>,
    ) -> Result<Self> {
        isolation.validate()?;
        let container = matches!(isolation.profile.as_str(), "appcontainer" | "lpac");
        ensure!(
            !container || std::env::var("PRTS_I1_EXPERIMENTAL").as_deref() == Ok("1"),
            "isolation: {} is not qualified",
            isolation.profile
        );
        let state = crate::isolation::state::State::create(container)
            .context("isolation: private state")?;
        let (command, cmd_child) = pipe(true)?;
        let (event, evt_child) = pipe(false)?;
        let (frame, frame_child) = pipe(false)?;
        let mut children = vec![handle(&cmd_child), handle(&evt_child), handle(&frame_child)];
        let provisioner = executable
            .file_stem()
            .is_some_and(|s| s == "isolation_desktop");
        let log_handle = if container || provisioner {
            let mut log = std::env::var("PRTS_BROWSER_LOG").unwrap_or_else(|_| {
                state
                    .path
                    .join("browser.log")
                    .to_string_lossy()
                    .into_owned()
            });
            if provisioner {
                log.push_str(".desktop.log");
            }
            let file = File::options().create(true).append(true).open(log)?;
            unsafe {
                SetHandleInformation(handle(&file), HANDLE_FLAG_INHERIT.0, HANDLE_FLAG_INHERIT)?;
            }
            children.push(handle(&file));
            Some(file)
        } else {
            None
        };
        let job = if let Some(job) = inherited_job {
            Some(job.try_clone()?)
        } else if isolation.profile == "none" {
            None
        } else {
            Some(
                crate::isolation::win::create_job(&isolation.job_limits)
                    .context("isolation: create job")?,
            )
        };
        let (desktop_helper, sandbox_desktop, browser_desktop) =
            if container && !extra_switches.contains(&"--i1-native-probe") {
                let helper_exe = std::env::current_exe()?.with_file_name("isolation_desktop.exe");
                let helper_config = crate::isolation::Config {
                    profile: "job".into(),
                    job_limits: isolation.job_limits.clone(),
                };
                let name = format!("--prts-desktop={}", state.name);
                let package = format!(
                    "--prts-package={}",
                    state.package.as_ref().context("package SID missing")?
                );
                let helper = Self::launch_internal(
                    &helper_exe,
                    &[&name, &package],
                    false,
                    &helper_config,
                    job.as_ref(),
                )
                .context("isolation: desktop provisioner launch")?;
                let mut event = helper.event.try_clone()?;
                let (tx, rx) = mpsc::sync_channel(1);
                let worker = thread::spawn(move || {
                    let _ = tx.send(Record::read(&mut event));
                });
                let result = rx.recv_timeout(Duration::from_secs(20));
                if result.is_err() {
                    helper.terminate();
                }
                let _ = worker.join();
                if result.as_ref().is_ok_and(|r| r.is_err()) {
                    let _ = helper.wait(Duration::from_secs(2));
                }
                let record = result
                    .context("isolation: desktop provisioner timeout")?
                    .context("isolation: desktop provisioner response; inspect .desktop.log")?;
                ensure!(
                    record.kind == 5 && record.id == 1,
                    "isolation: invalid desktop provisioner response"
                );
                let mut decoder = Decoder::new(&record.payload);
                let desktop = decoder.string()?;
                let browser_desktop = decoder.string()?;
                decoder.finish()?;
                (Some(Box::new(helper)), Some(desktop), Some(browser_desktop))
            } else {
                (None, None, None)
            };
        // Vec<usize> guarantees attribute-list pointer alignment. Windows returns its byte size.
        let mut bytes = 0;
        let attribute_count = 2 + u32::from(container) + u32::from(isolation.profile == "lpac");
        let _ =
            unsafe { InitializeProcThreadAttributeList(None, attribute_count, None, &mut bytes) };
        ensure!(bytes > 0, "No process attribute list size");
        let mut storage = vec![0usize; bytes.div_ceil(size_of::<usize>())];
        let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
        unsafe {
            InitializeProcThreadAttributeList(Some(list), attribute_count, None, &mut bytes)?;
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
        let internet = crate::isolation::state::Capability::from_string("S-1-15-3-1")?;
        let install = if container && std::env::var("PRTS_INSTALL_CAPABILITY").as_deref() == Ok("1")
        {
            Some(crate::isolation::state::Capability::named(
                "prtsInstallFiles",
            )?)
        } else {
            None
        };
        let mut capabilities = vec![internet.entry()];
        if let Some(install) = &install {
            capabilities.push(install.entry());
        }
        let security = windows::Win32::Security::SECURITY_CAPABILITIES {
            AppContainerSid: state.sid,
            Capabilities: capabilities.as_ptr() as _,
            CapabilityCount: capabilities.len() as u32,
            Reserved: 0,
        };
        let opt_out = 1u32; // PROCESS_CREATION_ALL_APPLICATION_PACKAGES_OPT_OUT
        if container {
            unsafe {
                UpdateProcThreadAttribute(
                    list,
                    0,
                    PROC_THREAD_ATTRIBUTE_SECURITY_CAPABILITIES as usize,
                    Some(&security as *const _ as _),
                    size_of_val(&security),
                    None,
                    None,
                )?;
                if isolation.profile == "lpac" {
                    UpdateProcThreadAttribute(
                        list,
                        0,
                        PROC_THREAD_ATTRIBUTE_ALL_APPLICATION_PACKAGES_POLICY as usize,
                        Some(&opt_out as *const _ as _),
                        size_of_val(&opt_out),
                        None,
                        None,
                    )?;
                }
            }
        }
        unsafe {
            UpdateProcThreadAttribute(
                list,
                0,
                PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
                Some(children.as_ptr().cast()),
                children.len() * size_of::<HANDLE>(),
                None,
                None,
            )?;
        }
        // Atomic job assignment closes the parent-death race between
        // CreateProcess and a subsequent AssignProcessToJobObject call.
        let jobs = [job.as_ref().map(handle).unwrap_or_default()];
        if job.is_some() {
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
        }
        let executable_text = executable.to_str().context("Chrome path is not UTF-8")?;
        ensure!(
            !executable_text.contains('"') && !executable_text.contains('\0'),
            "Invalid executable path"
        );
        let app: Vec<u16> = executable_text.encode_utf16().chain(Some(0)).collect();
        let mut text = format!(
            "\"{}\" {} --no-first-run --no-default-browser-check --disable-extensions --disable-component-update --disable-background-networking --disable-sync --prts-pipe=cmd:0x{:X},evt:0x{:X},frame:0x{:X}",
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
        text.push_str(&format!(
            " --user-data-dir=\"{}\" --disk-cache-dir=\"{}\"",
            state.path.join("profile").display(),
            state.path.join("cache").display()
        ));
        if container {
            text.push_str(" --enable-logging=stderr --v=1");
        } else if let Ok(log) = std::env::var("PRTS_BROWSER_LOG") {
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
        let private_desktop = browser_desktop.is_some();
        let mut browser_desktop: Vec<u16> = browser_desktop
            .unwrap_or_default()
            .encode_utf16()
            .chain(Some(0))
            .collect();
        if private_desktop {
            startup.StartupInfo.lpDesktop = PWSTR(browser_desktop.as_mut_ptr());
        }
        if let Some(log) = &log_handle {
            startup.StartupInfo.dwFlags |= STARTF_USESTDHANDLES;
            startup.StartupInfo.hStdInput = handle(&cmd_child);
            startup.StartupInfo.hStdOutput = handle(log);
            startup.StartupInfo.hStdError = handle(log);
        }
        startup.lpAttributeList = list;
        let mut process = PROCESS_INFORMATION::default();
        let mut environment = std::collections::BTreeMap::new();
        for key in [
            "SystemRoot",
            "WINDIR",
            "SystemDrive",
            "COMSPEC",
            "NUMBER_OF_PROCESSORS",
            "PROCESSOR_ARCHITECTURE",
            "USERPROFILE",
            "APPDATA",
            "LOCALAPPDATA",
        ] {
            if let Ok(value) = std::env::var(key) {
                environment.insert(key.to_owned(), value);
            }
        }
        for key in ["TEMP", "TMP"] {
            environment.insert(
                key.into(),
                state.path.join("temp").to_string_lossy().into_owned(),
            );
        }
        if let Some(desktop) = sandbox_desktop {
            environment.insert("PRTS_SANDBOX_DESKTOP".into(), desktop);
            environment.insert(
                "PRTS_TENANT_TEMP".into(),
                state.path.join("temp").to_string_lossy().into_owned(),
            );
        }
        let environment: Vec<u16> = environment
            .iter()
            .flat_map(|(k, v)| format!("{k}={v}\0").encode_utf16().collect::<Vec<_>>())
            .chain(Some(0))
            .collect();
        let cwd: Vec<u16> = state
            .path
            .to_string_lossy()
            .encode_utf16()
            .chain(Some(0))
            .collect();
        unsafe {
            CreateProcessW(
                PCWSTR(app.as_ptr()),
                Some(PWSTR(command_line.as_mut_ptr())),
                None,
                None,
                true,
                EXTENDED_STARTUPINFO_PRESENT
                    | CREATE_SUSPENDED
                    | CREATE_UNICODE_ENVIRONMENT
                    | if provisioner {
                        CREATE_NO_WINDOW
                    } else {
                        PROCESS_CREATION_FLAGS(0)
                    },
                Some(environment.as_ptr().cast()),
                PCWSTR(cwd.as_ptr()),
                &startup.StartupInfo,
                &mut process,
            )
            .context("isolation: CreateProcessW")?;
        }
        // CreateProcess grants full process access, including PROCESS_DUP_HANDLE and lifecycle rights.
        let process_handle = adopt(process.hProcess);
        let thread_handle = adopt(process.hThread);
        if let Some(job) = &job {
            let verified = crate::isolation::win::member(handle(&process_handle), handle(job));
            if !matches!(verified, Ok(true)) {
                unsafe {
                    let _ = TerminateProcess(handle(&process_handle), 1);
                }
                bail!("isolation: suspended browser job membership failed: {verified:?}");
            }
        }
        if unsafe { ResumeThread(handle(&thread_handle)) } == u32::MAX {
            unsafe {
                let _ = TerminateProcess(handle(&process_handle), 1);
            }
            bail!("Cannot resume contained Chromium");
        }
        drop(thread_handle);
        drop((cmd_child, evt_child, frame_child));
        drop(log_handle);
        Ok(Self {
            command: command.into(),
            event: event.into(),
            frame: frame.into(),
            job,
            process: process_handle,
            state,
            desktop_helper,
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
            if let Some(job) = &self.job {
                if !crate::isolation::win::terminate_job(handle(job), 1) {
                    self.state.cleanup_allowed.store(false, Ordering::Release);
                }
            } else {
                let _ = TerminateProcess(handle(&self.process), 1);
            }
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
        if self.pid != 0 && (self.job.is_some() || !self.wait(Duration::ZERO)) {
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
    job: Option<OwnedHandle>,
    process: OwnedHandle,
    state: Arc<crate::isolation::state::State>,
    _desktop_helper: Option<Box<RawBrowser>>,
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
    hardware_required: bool,
    active_gpu: Option<serde_json::Value>,
    container_required: bool,
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
            job: raw.job.as_ref().map(OwnedHandle::try_clone).transpose()?,
            process,
            state: raw.state.clone(),
            _desktop_helper: raw.desktop_helper.take(),
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
            hardware_required: false,
            active_gpu: None,
            container_required: false,
            transcript: VecDeque::new(),
        };
        // RawBrowser::drop skips termination for a successfully transferred launch.
        Ok(session)
    }
    fn verify_container_tree(&self) -> Result<()> {
        let Some(package) = self.state.package.as_ref() else {
            return Ok(());
        };
        let job = self
            .job
            .as_ref()
            .context("isolation: container missing job")?;
        let helper_pid = self._desktop_helper.as_ref().map(|h| h.pid);
        let install = crate::isolation::state::Capability::named("prtsInstallFiles")?;
        let install_sid = crate::isolation::state::sid_string(install.entry().Sid)?;
        for pid in crate::isolation::win::processes(handle(job))? {
            if Some(pid) == helper_pid {
                continue;
            } // authenticated, broker-owned fixed-purpose helper
            let token = match crate::isolation::win::token_evidence(pid) {
                Ok(v) => v,
                Err(e)
                    if e.downcast_ref::<windows::core::Error>()
                        .is_some_and(|e| e.code() == windows::core::HRESULT::from_win32(87)) =>
                {
                    continue;
                }
                Err(e) => return Err(e),
            };
            let integrity = token["integrity"]
                .as_str()
                .and_then(|s| s.rsplit('-').next())
                .and_then(|s| s.parse::<u32>().ok());
            ensure!(
                token["appcontainer"] == true
                    && token["package_sid"] == *package
                    && integrity.is_some_and(|v| v <= 4096),
                "isolation: child token escaped tenant identity/integrity: {token}"
            );
            ensure!(
                token["capabilities"]
                    .as_array()
                    .context("capabilities missing")?
                    .iter()
                    .all(|v| v["sid"] == "S-1-15-3-1" || v["sid"] == install_sid),
                "isolation: child gained an unexpected capability: {token}"
            );
        }
        Ok(())
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
            if let Some(job) = &self.job {
                let _ = TerminateJobObject(handle(job), 1);
            } else {
                let _ = TerminateProcess(handle(&self.process), 1);
            }
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
                    ensure!(
                        scene.layers.len() == 1,
                        "Single-window adapter does not compose auxiliary layers"
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
                        ensure!(
                            layer.role == 1
                                && layer.parent == 0
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
            return self.enqueue(reply.encode_version(self.major)?);
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
        if let Some(job) = &self.job
            && !crate::isolation::win::terminate_job(handle(job), 0)
        {
            self.state.cleanup_allowed.store(false, Ordering::Release);
        }
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
    pub fn launch_isolated(
        executable: impl AsRef<Path>,
        cancel: &AtomicBool,
        isolation: &crate::isolation::Config,
    ) -> Result<Self> {
        Self::connect_internal(
            RawBrowser::launch_isolated(executable.as_ref(), &[], true, isolation)
                .context("isolation: requested launch failed")?,
            Some(cancel),
        )
    }
    pub fn isolation_stats(&self) -> Result<serde_json::Value> {
        let session = self
            .session
            .lock()
            .map_err(|_| anyhow!("Session poisoned"))?;
        match &session.job {
            Some(job) => {
                let mut stats = crate::isolation::win::accounting(handle(job))?;
                stats["pids"] = serde_json::json!(crate::isolation::win::processes(handle(job))?);
                stats["browser_pid"] = serde_json::json!(session.pid);
                stats["state_path"] = serde_json::json!(session.state.path);
                stats["active_gpu"] = serde_json::json!(session.active_gpu);
                Ok(stats)
            }
            None => Ok(
                serde_json::json!({"profile":"none","browser_pid":session.pid,"state_path":session.state.path,"active_gpu":session.active_gpu,
                "pids":crate::isolation::win::descendants(session.pid)?}),
            ),
        }
    }
    pub fn process_ids(&self) -> Result<Vec<u32>> {
        let session = self
            .session
            .lock()
            .map_err(|_| anyhow!("Session poisoned"))?;
        match &session.job {
            Some(job) => crate::isolation::win::processes(handle(job)),
            None => crate::isolation::win::descendants(session.pid),
        }
    }
    pub fn state_path(&self) -> std::path::PathBuf {
        self.session.lock().unwrap().state.path.clone()
    }
    pub fn gpu_info(&self) -> Result<serde_json::Value> {
        let record = self.request(9, Vec::new(), 10)?;
        Self::decode_gpu_info(record)
    }
    fn decode_gpu_info(record: Record) -> Result<serde_json::Value> {
        let mut d = Decoder::new(&record.payload);
        let pid = d.u32()?;
        let vendor = d.u32()?;
        let device = d.u32()?;
        let luid = d.u64()?;
        let hardware = d.u8()? != 0;
        let swiftshader = d.u8()? != 0;
        let sandboxed = d.u8()? != 0;
        let driver = d.string()?;
        let renderer = d.string()?;
        let version = d.string()?;
        let n = d.u32()?;
        ensure!(n <= 64, "Too many GPU feature statuses");
        let mut features = Vec::new();
        for _ in 0..n {
            features.push(d.u32()?);
        }
        d.finish()?;
        Ok(
            serde_json::json!({"pid":pid,"vendor":vendor,"device":device,"luid":format!("{luid:016x}"),
            "hardware":hardware,"swiftshader":swiftshader,"sandboxed":sandboxed,"driver":driver,
            "renderer":renderer,"version":version,"features":features}),
        )
    }
    pub fn require_hardware_gpu(&self) -> Result<serde_json::Value> {
        let info = self.gpu_info()?;
        Self::validate_hardware_gpu(&info)?;
        Ok(info)
    }
    fn validate_hardware_gpu(info: &serde_json::Value) -> Result<()> {
        let renderer = info["renderer"].as_str().unwrap_or_default().to_lowercase();
        let luid = crate::d3d::shared()?.lock().unwrap().luid;
        ensure!(
            info["hardware"] == true
                && info["swiftshader"] == false
                && info["sandboxed"] == true
                && info["pid"].as_u64().is_some_and(|v| v > 0)
                && info["vendor"] == 0x8086
                && info["device"] == 0x9bc5
                && info["luid"] == format!("{luid:016x}")
                && renderer.contains("angle")
                && renderer.contains("direct3d11")
                && !["swiftshader", "warp", "basic render", "software"]
                    .iter()
                    .any(|v| renderer.contains(v))
                && info["features"][1] == 0
                && info["features"][6] == 0,
            "isolation: hardware GPU gate rejected active report: {info}"
        );
        Ok(())
    }
    /// Production readiness and continuous GPU-generation validation.
    pub fn enforce_hardware_gpu(&self, cancel: &AtomicBool) -> Result<()> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            ensure!(
                !cancel.load(Ordering::Acquire),
                "isolation: startup cancelled"
            );
            match self.require_hardware_gpu() {
                Ok(info) => {
                    let mut session = self
                        .session
                        .lock()
                        .map_err(|_| anyhow!("Session poisoned"))?;
                    eprintln!("prtsd: qualified active GPU {info}");
                    session.active_gpu = Some(info);
                    session.hardware_required = true;
                    session.verify_container_tree()?;
                    session.container_required = session.state.package.is_some();
                    return Ok(());
                }
                Err(e) if Instant::now() >= deadline => return Err(e),
                Err(_) => thread::sleep(Duration::from_millis(100)),
            }
        }
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
                if session.hardware_required {
                    let result = session
                        .request(9, Vec::new(), 10)
                        .and_then(Browser::decode_gpu_info)
                        .and_then(|info| {
                            Browser::validate_hardware_gpu(&info)?;
                            Ok(info)
                        });
                    match result {
                        Ok(info) => session.active_gpu = Some(info),
                        Err(e) => {
                            let _: Result<()> = session
                                .fail(format!("isolation: active GPU revalidation failed: {e:#}"));
                            break;
                        }
                    }
                }
                if session.container_required
                    && let Err(e) = session.verify_container_tree()
                {
                    let _: Result<()> = session.fail(format!(
                        "isolation: tenant token revalidation failed: {e:#}"
                    ));
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
        while let Some(mut frame) = s.frames.take_frame() {
            if s.major == 2 {
                let owner = s.scenes.values().find(|scene| {
                    scene
                        .layers
                        .iter()
                        .any(|l| l.source == frame.target && l.generation == frame.generation)
                });
                let Some(scene) = owner else {
                    continue;
                };
                frame.target = scene.window;
                frame.generation = scene.generation;
            }
            return Ok(Some(frame));
        }
        Ok(None)
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
        let Some(job) = s.job.as_ref() else {
            let mut birth = Default::default();
            let mut exit = Default::default();
            let mut kernel = windows::Win32::Foundation::FILETIME::default();
            let mut user = windows::Win32::Foundation::FILETIME::default();
            unsafe {
                GetProcessTimes(
                    handle(&s.process),
                    &mut birth,
                    &mut exit,
                    &mut kernel,
                    &mut user,
                )?;
            }
            let ticks = |v: windows::Win32::Foundation::FILETIME| {
                (u64::from(v.dwHighDateTime) << 32) | u64::from(v.dwLowDateTime)
            };
            return Ok((
                (ticks(kernel) + ticks(user)) as f64 / 10_000_000.,
                0,
                s.frames.resource_count(),
            ));
        };
        unsafe {
            QueryInformationJobObject(
                Some(handle(job)),
                JobObjectBasicAccountingInformation,
                &mut cpu as *mut _ as *mut _,
                size_of_val(&cpu) as u32,
                None,
            )?;
            QueryInformationJobObject(
                Some(handle(job)),
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
