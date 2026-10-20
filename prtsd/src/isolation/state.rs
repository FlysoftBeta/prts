//! Broker-owned ephemeral state; never follow a tenant-created reparse point at cleanup.
use super::*;
use std::{
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
};
use windows::{
    Win32::{
        Foundation::{HANDLE, HLOCAL, LocalFree},
        Security::{Authorization::*, Isolation::*, *},
        System::{Com::CoCreateGuid, Threading::*},
    },
    core::{PCWSTR, PWSTR},
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
pub fn sid_string(sid: PSID) -> Result<String> {
    let mut out = PWSTR::null();
    unsafe {
        ConvertSidToStringSidW(sid, &mut out)?;
        let text = out.to_string();
        LocalFree(Some(HLOCAL(out.0.cast())));
        Ok(text?)
    }
}
pub fn token_info(token: HANDLE, class: TOKEN_INFORMATION_CLASS) -> Result<Vec<usize>> {
    let mut size = 0;
    unsafe {
        let _ = GetTokenInformation(token, class, None, 0, &mut size);
    }
    ensure!(size > 0, "no token information for {}", class.0);
    let mut data = vec![0usize; (size as usize).div_ceil(size_of::<usize>())];
    unsafe {
        GetTokenInformation(
            token,
            class,
            Some(data.as_mut_ptr().cast()),
            size,
            &mut size,
        )?;
    }
    Ok(data)
}
pub fn current_user() -> Result<String> {
    let mut raw = HANDLE::default();
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut raw)?;
    }
    let token = unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw.0) };
    let info = token_info(super::win::handle(&token), TokenUser)?;
    sid_string(unsafe { (*(info.as_ptr() as *const TOKEN_USER)).User.Sid })
}
pub struct Capability {
    storage: Vec<usize>,
}
impl Capability {
    pub fn named(name: &str) -> Result<Self> {
        let name = wide(name);
        let mut groups = std::ptr::null_mut();
        let mut group_count = 0;
        let mut caps = std::ptr::null_mut();
        let mut cap_count = 0;
        unsafe {
            DeriveCapabilitySidsFromName(
                PCWSTR(name.as_ptr()),
                &mut groups,
                &mut group_count,
                &mut caps,
                &mut cap_count,
            )?;
            let text = if cap_count == 1 {
                sid_string(*caps)
            } else {
                Err(anyhow::anyhow!("unexpected named capability SID count"))
            };
            for (sids, count) in [(groups, group_count), (caps, cap_count)] {
                for i in 0..count {
                    LocalFree(Some(HLOCAL((*sids.add(i as usize)).0)));
                }
                LocalFree(Some(HLOCAL(sids.cast())));
            }
            Self::from_string(&text?)
        }
    }
    pub fn from_string(text: &str) -> Result<Self> {
        let text = wide(text);
        let mut sid = PSID::default();
        unsafe {
            ConvertStringSidToSidW(PCWSTR(text.as_ptr()), &mut sid)?;
            let len = GetLengthSid(sid);
            let mut storage = vec![0usize; (len as usize).div_ceil(size_of::<usize>())];
            let result = CopySid(len, PSID(storage.as_mut_ptr().cast()), sid);
            LocalFree(Some(HLOCAL(sid.0)));
            result?;
            Ok(Self { storage })
        }
    }
    pub fn entry(&self) -> SID_AND_ATTRIBUTES {
        SID_AND_ATTRIBUTES {
            Sid: PSID(self.storage.as_ptr() as _),
            Attributes: 4, /* SE_GROUP_ENABLED */
        }
    }
}
fn acl(path: &Path, user: &str, package: Option<&str>) -> Result<()> {
    // Protected DACL: broker, administrators, SYSTEM and this package only.
    let package = package
        .map(|s| format!("(D;OIIO;0x20;;;{s})(A;OICI;0x1301bf;;;{s})"))
        .unwrap_or_default();
    let descriptor = wide(&format!(
        "D:P(A;OICI;FA;;;SY)(A;OICI;FA;;;BA)(A;OICI;FA;;;{user}){package}S:(ML;OICI;NW;;;LW)"
    ));
    let path = wide(path.to_str().context("state path UTF-8")?);
    let mut sd = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(descriptor.as_ptr()),
            1,
            &mut sd,
            None,
        )?;
        let result = SetFileSecurityW(
            PCWSTR(path.as_ptr()),
            DACL_SECURITY_INFORMATION
                | LABEL_SECURITY_INFORMATION
                | PROTECTED_DACL_SECURITY_INFORMATION,
            sd,
        )
        .ok();
        LocalFree(Some(HLOCAL(sd.0)));
        result?;
    }
    Ok(())
}
pub struct State {
    pub path: PathBuf,
    pub name: String,
    pub package: Option<String>,
    pub sid: PSID,
    marker: PathBuf,
    pub cleanup_allowed: std::sync::atomic::AtomicBool,
}
// SID storage is immutable and owned until the last process/session reference is gone.
unsafe impl Send for State {}
unsafe impl Sync for State {}
impl State {
    pub fn create(container: bool) -> Result<Arc<Self>> {
        let root = PathBuf::from(std::env::var("LOCALAPPDATA")?)
            .join("prts")
            .join("instances");
        std::fs::create_dir_all(&root)?;
        let user = current_user()?;
        acl(&root, &user, None)?;
        let id = unsafe { CoCreateGuid()? };
        let name = format!("prts.{id:?}").to_ascii_lowercase();
        let path = root.join(&name);
        std::fs::create_dir(&path)?;
        let marker = root.join(format!("{name}.owner.json"));
        let mut state = Self {
            path,
            name,
            package: None,
            sid: PSID::default(),
            marker,
            cleanup_allowed: std::sync::atomic::AtomicBool::new(true),
        };
        std::fs::write(
            &state.marker,
            serde_json::to_vec(
                &serde_json::json!({"pid":std::process::id(),"created":process_birth(unsafe { GetCurrentProcess() })?,"container":container}),
            )?,
        )?;
        if container {
            let name = wide(&state.name);
            state.sid = unsafe {
                CreateAppContainerProfile(
                    PCWSTR(name.as_ptr()),
                    PCWSTR(name.as_ptr()),
                    PCWSTR(name.as_ptr()),
                    None,
                )?
            };
            state.package = Some(sid_string(state.sid)?);
        }
        acl(&state.path, &user, state.package.as_deref())?;
        for dir in ["profile", "cache", "temp", "downloads"] {
            std::fs::create_dir(state.path.join(dir))?;
        }
        std::fs::create_dir(state.path.join("profile").join("Crashpad"))?;
        Ok(Arc::new(state))
    }
}
fn process_birth(process: HANDLE) -> Result<u64> {
    use windows::Win32::Foundation::FILETIME;
    let mut birth = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    unsafe {
        GetProcessTimes(process, &mut birth, &mut exit, &mut kernel, &mut user)?;
    }
    Ok((u64::from(birth.dwHighDateTime) << 32) | u64::from(birth.dwLowDateTime))
}
/// Only broker-created manifests in a protected root authorize stale cleanup.
pub fn cleanup_stale() -> Result<()> {
    let root = PathBuf::from(std::env::var("LOCALAPPDATA")?)
        .join("prts")
        .join("instances");
    if !root.exists() {
        return Ok(());
    }
    ensure!(
        std::fs::symlink_metadata(&root)?.file_attributes() & 0x400 == 0,
        "isolation root is a reparse point"
    );
    acl(&root, &current_user()?, None)?;
    for entry in std::fs::read_dir(&root)? {
        let entry = entry?;
        let file = entry.file_name().to_string_lossy().into_owned();
        let Some(name) = file.strip_suffix(".owner.json") else {
            continue;
        };
        let Some(id) = name.strip_prefix("prts.") else {
            continue;
        };
        if id.len() != 36 || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
            continue;
        }
        ensure!(
            entry.file_type()?.is_file() && !entry.file_type()?.is_symlink(),
            "invalid owner manifest"
        );
        let owner: serde_json::Value = serde_json::from_slice(&std::fs::read(entry.path())?)?;
        let pid = owner["pid"].as_u64().context("owner pid")? as u32;
        let birth = owner["created"].as_u64().context("owner birth")?;
        let live = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
                false,
                pid,
            )
        };
        match live {
            Ok(process) => {
                let process =
                    unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(process.0) };
                if process_birth(super::win::handle(&process))? == birth
                    && unsafe { WaitForSingleObject(super::win::handle(&process), 0) }
                        == windows::Win32::Foundation::WAIT_TIMEOUT
                {
                    continue;
                }
            }
            Err(e) if e.code() == windows::core::HRESULT::from_win32(87) => {}
            Err(e) => {
                eprintln!("prtsd: cannot establish stale owner {pid}: {e}");
                continue;
            }
        }
        // name is a validated single path component; root is broker-owned, never supplied by a tenant.
        let target = root.join(name);
        ensure!(
            target.parent() == Some(root.as_path()),
            "cleanup outside root"
        );
        if target.exists() {
            cleanup_tree(&target).with_context(|| format!("cleanup stale {}", target.display()))?;
        }
        if owner["container"] == true {
            let text = wide(name);
            if let Err(e) = unsafe { DeleteAppContainerProfile(PCWSTR(text.as_ptr())) } {
                eprintln!("prtsd: stale profile {name}: {e}");
                continue;
            }
        }
        std::fs::remove_file(entry.path())?;
        eprintln!("prtsd: cleaned stale isolation state {name}");
    }
    Ok(())
}
fn remove_tree(path: &Path) -> Result<()> {
    let meta = match std::fs::symlink_metadata(path) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    if meta.file_attributes() & 0x400 != 0 {
        if meta.is_dir() {
            std::fs::remove_dir(path)?;
        } else {
            std::fs::remove_file(path)?;
        }
    } else if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            remove_tree(&entry?.path())?;
        }
        std::fs::remove_dir(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}
fn cleanup_tree(path: &Path) -> Result<()> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        match remove_tree(path) {
            Ok(()) => return Ok(()),
            Err(e)
                if e.downcast_ref::<std::io::Error>()
                    .is_some_and(|e| matches!(e.raw_os_error(), Some(32 | 33 | 145)))
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(50))
            }
            Err(e) => return Err(e),
        }
    }
}
impl Drop for State {
    fn drop(&mut self) {
        if !self
            .cleanup_allowed
            .load(std::sync::atomic::Ordering::Acquire)
        {
            eprintln!(
                "prtsd: retain isolation owner manifest for undrained tree {}",
                self.name
            );
            if !self.sid.0.is_null() {
                unsafe {
                    FreeSid(self.sid);
                }
            }
            return;
        }
        let mut cleaned = true;
        if let Err(e) = cleanup_tree(&self.path) {
            cleaned = false;
            eprintln!(
                "prtsd: isolation state cleanup {}: {e:#}",
                self.path.display()
            );
        }
        if !self.sid.0.is_null() {
            let name = wide(&self.name);
            unsafe {
                if let Err(e) = DeleteAppContainerProfile(PCWSTR(name.as_ptr())) {
                    cleaned = false;
                    eprintln!("prtsd: delete {}: {e}", self.name);
                }
                FreeSid(self.sid);
            }
        }
        if cleaned {
            let _ = std::fs::remove_file(&self.marker);
        }
    }
}
use std::os::windows::io::FromRawHandle;
