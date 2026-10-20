//! Shared M1/M3 native desktop and Explorer taskbar acceptance probe.
use anyhow::{Result, ensure};
use std::{
    collections::{BTreeSet, HashSet},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        Foundation::{HWND, LPARAM},
        Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute},
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize,
            },
            Threading::{AttachThreadInput, GetCurrentThreadId},
        },
        UI::{
            Accessibility::{
                CUIAutomation, IUIAutomation, TreeScope_Descendants, UIA_ButtonControlTypeId,
            },
            WindowsAndMessaging::*,
        },
    },
    core::BOOL,
};
#[derive(Clone, Debug)]
struct Window {
    hwnd: usize,
    pid: u32,
    visible: bool,
    cloaked: u32,
    style: isize,
    ex_style: isize,
    title: String,
    class: String,
}
unsafe extern "system" fn enumerate(hwnd: HWND, param: LPARAM) -> BOOL {
    let windows = unsafe { &mut *(param.0 as *mut Vec<Window>) };
    let mut pid = 0;
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }
    let mut cloaked = 0u32;
    let cloak =
        unsafe { DwmGetWindowAttribute(hwnd, DWMWA_CLOAKED, (&mut cloaked as *mut u32).cast(), 4) };
    let mut title = vec![0u16; 1024];
    let n = unsafe { GetWindowTextW(hwnd, &mut title) };
    let mut class = vec![0u16; 256];
    let class_n = unsafe { GetClassNameW(hwnd, &mut class) };
    windows.push(Window {
        hwnd: hwnd.0 as usize,
        pid,
        visible: unsafe { IsWindowVisible(hwnd).as_bool() },
        cloaked: if cloak.is_ok() { cloaked } else { 0 },
        style: unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) },
        ex_style: unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) },
        title: String::from_utf16_lossy(&title[..n as usize]),
        class: String::from_utf16_lossy(&class[..class_n as usize]),
    });
    true.into()
}
fn windows() -> Result<Vec<Window>> {
    let mut list = Vec::new();
    unsafe {
        EnumWindows(
            Some(enumerate),
            LPARAM((&mut list as *mut Vec<Window>) as isize),
        )?;
    }
    Ok(list)
}
fn taskbar_buttons() -> Result<BTreeSet<String>> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
    }
    struct Com;
    impl Drop for Com {
        fn drop(&mut self) {
            unsafe {
                CoUninitialize();
            }
        }
    }
    let _com = Com;
    let automation: IUIAutomation =
        unsafe { CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER) }?;
    let condition = unsafe { automation.CreateTrueCondition() }?;
    let mut buttons = BTreeSet::new();
    let mut trays = 0;
    for window in windows()?
        .iter()
        .filter(|w| w.class == "Shell_TrayWnd" || w.class == "Shell_SecondaryTrayWnd")
    {
        trays += 1;
        let root = unsafe { automation.ElementFromHandle(HWND(window.hwnd as *mut _)) }?;
        let elements = unsafe { root.FindAll(TreeScope_Descendants, &condition) }?;
        for i in 0..unsafe { elements.Length() }? {
            let element = unsafe { elements.GetElement(i) }?;
            let name = unsafe { element.CurrentName() }?.to_string();
            let class = unsafe { element.CurrentClassName() }?.to_string();
            let lower = name.to_ascii_lowercase();
            if (unsafe { element.CurrentControlType() }? == UIA_ButtonControlTypeId
                && class.to_ascii_lowercase().contains("task"))
                || lower.contains("chromium")
                || lower.contains("google chrome")
                || lower.contains("prts-m1-navigation")
            {
                buttons.insert(format!("{class}: {name}"));
            }
        }
    }
    ensure!(
        trays > 0,
        "Explorer taskbar HWND not found for taskbar visibility check"
    );
    Ok(buttons)
}
struct NotepadSentinel {
    child: std::process::Child,
    file: std::path::PathBuf,
}
impl Drop for NotepadSentinel {
    fn drop(&mut self) {
        if let Some(token) = self.file.file_name()
            && let Ok(list) = windows()
        {
            for window in list
                .iter()
                .filter(|w| w.title.contains(token.to_string_lossy().as_ref()))
            {
                unsafe {
                    let _ = PostMessageW(
                        Some(HWND(window.hwnd as *mut _)),
                        WM_CLOSE,
                        Default::default(),
                        Default::default(),
                    );
                }
            }
        }
        let _ = self.child.try_wait();
        let _ = std::fs::remove_file(&self.file);
    }
}
fn cursor_state() -> Result<Option<(u32, usize, i32, i32)>> {
    let mut info = CURSORINFO {
        cbSize: size_of::<CURSORINFO>() as u32,
        ..Default::default()
    };
    unsafe {
        if let Err(e) = GetCursorInfo(&mut info) {
            if e.code().0 == 0x80070005u32 as i32
                && std::env::var("PRTS_M1_ALLOW_DISCONNECTED_DESKTOP").as_deref() == Ok("1")
            {
                return Ok(None);
            }
            return Err(e.into());
        }
    }
    Ok(Some((
        info.flags.0,
        info.hCursor.0 as usize,
        info.ptScreenPos.x,
        info.ptScreenPos.y,
    )))
}
pub struct DesktopProbe {
    cursor_baseline: Option<(u32, usize, i32, i32)>,
    cursor_changed: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    sampler: Option<thread::JoinHandle<()>>,
    changes: Arc<Mutex<Vec<usize>>>,
    sentinel: usize,
    focus_verified: bool,
    _notepad: NotepadSentinel,
    baseline: HashSet<usize>,
    taskbar_baseline: BTreeSet<String>,
}
impl DesktopProbe {
    pub fn start() -> Result<Self> {
        let file = std::env::temp_dir().join(format!("prts-m1-focus-{}.txt", std::process::id()));
        std::fs::write(
            &file,
            b"PRTS M1 focus sentinel. No input is sent to this application.\r\n",
        )?;
        let notepad = std::process::Command::new("notepad.exe")
            .arg(&file)
            .spawn()?;
        let notepad = NotepadSentinel {
            child: notepad,
            file,
        };
        let token = notepad.file.file_name().unwrap().to_string_lossy();
        let until = Instant::now() + Duration::from_secs(10);
        let sentinel = loop {
            if let Some(w) = windows()?
                .iter()
                .find(|w| w.visible && w.title.contains(token.as_ref()))
            {
                break w.hwnd;
            }
            ensure!(
                Instant::now() < until,
                "Notepad focus sentinel HWND not found"
            );
            thread::sleep(Duration::from_millis(100));
        };
        unsafe {
            let foreground = GetForegroundWindow();
            let foreground_thread = GetWindowThreadProcessId(foreground, None);
            let current_thread = GetCurrentThreadId();
            let attached = foreground_thread != 0
                && foreground_thread != current_thread
                && AttachThreadInput(current_thread, foreground_thread, true).as_bool();
            let _ = ShowWindow(HWND(sentinel as *mut _), SW_RESTORE);
            let _ = SetForegroundWindow(HWND(sentinel as *mut _));
            if attached {
                let _ = AttachThreadInput(current_thread, foreground_thread, false);
            }
        }
        thread::sleep(Duration::from_millis(300));
        let foreground = unsafe { GetForegroundWindow().0 as usize };
        let focus_verified = foreground == sentinel;
        ensure!(
            focus_verified
                || (foreground == 0
                    && std::env::var("PRTS_M1_ALLOW_DISCONNECTED_DESKTOP").as_deref() == Ok("1")),
            "Could not establish Notepad foreground focus (wanted0x{sentinel:X},got0x{foreground:X}); reconnect the Windows session or explicitly set PRTS_M1_ALLOW_DISCONNECTED_DESKTOP=1 to mark this check UNVERIFIED"
        );
        if !focus_verified {
            println!(
                "DESKTOP foreground check UNVERIFIED: GetForegroundWindow=NULL in disconnected Windows session; no interactive focus baseline exists"
            );
        }
        let expected_foreground = foreground;
        let cursor_baseline = cursor_state()?;
        if cursor_baseline.is_none() {
            println!(
                "DESKTOP physical cursor observation UNVERIFIED: GetCursorInfo ACCESS_DENIED on disconnected input desktop; require browser native-write suppression counters separately"
            );
        }
        let cursor_changed = Arc::new(AtomicBool::new(false));
        let cursor_sampler = cursor_changed.clone();
        let baseline = windows()?.iter().map(|w| w.hwnd).collect();
        let taskbar_baseline = taskbar_buttons()?;
        println!("DESKTOP Explorer UIAutomation taskbar baseline: {taskbar_baseline:?}");
        let stop = Arc::new(AtomicBool::new(false));
        let changes = Arc::new(Mutex::new(Vec::new()));
        let s = stop.clone();
        let c = changes.clone();
        let sampler = thread::spawn(move || {
            while !s.load(Ordering::Acquire) {
                if cursor_baseline.is_some()
                    && cursor_state().is_ok_and(|state| state != cursor_baseline)
                {
                    cursor_sampler.store(true, Ordering::Release);
                }
                let fg = unsafe { GetForegroundWindow().0 as usize };
                if fg != expected_foreground {
                    let mut v = c.lock().unwrap();
                    if v.last() != Some(&fg) {
                        v.push(fg);
                    }
                }
                thread::sleep(Duration::from_millis(10));
            }
        });
        println!("DESKTOP sentinel=Notepad hwnd=0x{sentinel:X}; foreground sampled every 10ms");
        Ok(Self {
            cursor_baseline,
            cursor_changed,
            stop,
            sampler: Some(sampler),
            changes,
            sentinel,
            focus_verified,
            _notepad: notepad,
            baseline,
            taskbar_baseline,
        })
    }
    pub fn check(&self, pid: u32, stage: &str) -> Result<()> {
        ensure!(
            cursor_state()? == self.cursor_baseline && !self.cursor_changed.load(Ordering::Acquire),
            "OS cursor shape/visibility/position changed during {stage}"
        );
        if self.cursor_baseline.is_some() {
            println!(
                "DESKTOP {stage}: OS cursor shape/visibility/position unchanged (sampled every10ms)"
            );
        }
        let snapshot = windows()?;
        let owned: Vec<_> = snapshot.iter().filter(|w| w.pid == pid).collect();
        for w in &owned {
            println!(
                "DESKTOP {stage}: hwnd=0x{:X} visible={} DWMWA_CLOAKED={} WS_STYLE=0x{:X} WS_EXSTYLE=0x{:X} title={:?}",
                w.hwnd, w.visible, w.cloaked, w.style, w.ex_style, w.title
            );
            ensure!(
                !w.visible || w.cloaked != 0,
                "Visible uncloaked PRTS top-level window"
            );
        }
        let new_visible: Vec<_> = snapshot
            .iter()
            .filter(|w| {
                !self.baseline.contains(&w.hwnd) && w.pid == pid && w.visible && w.cloaked == 0
            })
            .collect();
        ensure!(new_visible.is_empty(), "New visible browser windows");
        let taskbar = taskbar_buttons()?;
        let added: Vec<_> = taskbar.difference(&self.taskbar_baseline).collect();
        ensure!(
            added.is_empty(),
            "New or changed Explorer taskbar browser/running-app buttons: {added:?}"
        );
        println!("DESKTOP {stage}: Explorer UIAutomation taskbar buttons unchanged ({taskbar:?})");
        ensure!(
            unsafe { GetForegroundWindow().0 as usize }
                == if self.focus_verified {
                    self.sentinel
                } else {
                    0
                },
            "Foreground changed at {stage}"
        );
        let changes = self.changes.lock().unwrap().clone();
        ensure!(
            changes.is_empty(),
            "Foreground changed during PRTS operation: {changes:?}"
        );
        println!(
            "DESKTOP {stage}: {} browser HWNDs, 0 visible/uncloaked, 0 visible taskbar-eligible windows; foreground {}",
            owned.len(),
            if self.focus_verified {
                "unchanged"
            } else {
                "UNVERIFIED (disconnected desktop)"
            }
        );
        Ok(())
    }
}
impl Drop for DesktopProbe {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(h) = self.sampler.take() {
            let _ = h.join();
        }
    }
}
