use super::*;
use std::{
    mem::size_of,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
};
use windows::{
    Win32::{
        Foundation::{GetHandleInformation, HANDLE, HANDLE_FLAG_INHERIT},
        System::{JobObjects::*, Threading::*},
    },
    core::BOOL,
};

pub fn handle(value: &impl AsRawHandle) -> HANDLE {
    HANDLE(value.as_raw_handle())
}
pub fn native_probe() -> Result<serde_json::Value> {
    use windows::Win32::{Security::GetUserObjectSecurity, System::StationsAndDesktops::*};
    let mut out = serde_json::Map::new();
    if let Some(address) =
        std::env::args().find_map(|a| a.strip_prefix("--i1-connect=").map(str::to_owned))
    {
        use std::net::ToSocketAddrs;
        let mut sockets = Vec::new();
        for target in [address.as_str(), "example.org:80"] {
            let result = (|| -> std::io::Result<()> {
                let address = target
                    .to_socket_addrs()?
                    .next()
                    .ok_or_else(|| std::io::Error::other("no address"))?;
                std::net::TcpStream::connect_timeout(&address, std::time::Duration::from_secs(3))?;
                Ok(())
            })();
            sockets.push(serde_json::json!({"target":target,"allowed":result.is_ok(),"error":result.err().and_then(|e|e.raw_os_error())}));
        }
        out.insert("sockets".into(), serde_json::json!(sockets));
    }
    unsafe {
        let station = GetProcessWindowStation()?;
        let desk = GetThreadDesktop(GetCurrentThreadId())?;
        for (label, object) in [
            ("station_security", HANDLE(station.0)),
            ("desktop_security", HANDLE(desk.0)),
        ] {
            let mut size = 0;
            let result = GetUserObjectSecurity(object, &4u32, None, 0, &mut size);
            out.insert(
                label.into(),
                serde_json::json!({"size":size,"hresult":result.err().map(|e|e.code().0)}),
            );
        }
        let station = CreateWindowStationW(windows::core::PCWSTR::null(), 0, 0x0008 | 0x0002, None);
        let station_error = match station {
            Ok(s) => {
                CloseWindowStation(s)?;
                None
            }
            Err(e) => Some(e.code().0),
        };
        out.insert("create_station".into(), serde_json::json!(station_error));
        let named: Vec<u16> = format!("prts.probe.{}\0", std::process::id())
            .encode_utf16()
            .collect();
        let station = CreateWindowStationW(windows::core::PCWSTR(named.as_ptr()), 0, 0xa, None);
        let station_error = match station {
            Ok(s) => {
                CloseWindowStation(s)?;
                None
            }
            Err(e) => Some(e.code().0),
        };
        out.insert(
            "create_named_station".into(),
            serde_json::json!(station_error),
        );
        let name: Vec<u16> = format!("prts_i1_probe_{}\0", std::process::id())
            .encode_utf16()
            .collect();
        let desk = CreateDesktopW(
            windows::core::PCWSTR(name.as_ptr()),
            windows::core::PCWSTR::null(),
            None,
            DESKTOP_CONTROL_FLAGS(0),
            0x0002 | 0x0001 | 0x20000 | 0x40000 | 0x80000,
            None,
        );
        let desk_error = match desk {
            Ok(d) => {
                CloseDesktop(d)?;
                None
            }
            Err(e) => Some(e.code().0),
        };
        out.insert("create_desktop".into(), serde_json::json!(desk_error));
    }
    out.insert("token".into(), token_evidence(std::process::id())?);
    match crate::d3d::shared() {
        Ok(gpu) => {
            out.insert(
                "hardware_d3d11_luid".into(),
                serde_json::json!(format!("{:016x}", gpu.lock().unwrap().luid)),
            );
        }
        Err(e) => {
            out.insert(
                "hardware_d3d11_error".into(),
                serde_json::json!(format!("{e:#}")),
            );
        }
    }
    Ok(out.into())
}
pub fn create_job(limits: &JobLimits) -> Result<OwnedHandle> {
    limits.validate()?;
    let job = unsafe { CreateJobObjectW(None, windows::core::PCWSTR::null())? };
    let job = unsafe { OwnedHandle::from_raw_handle(job.0) };
    let mut handle_flags = 0;
    unsafe {
        GetHandleInformation(handle(&job), &mut handle_flags)?;
    }
    ensure!(
        handle_flags & HANDLE_FLAG_INHERIT.0 == 0,
        "job handle is inheritable"
    );
    let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
        | JOB_OBJECT_LIMIT_PROCESS_MEMORY
        | JOB_OBJECT_LIMIT_JOB_MEMORY;
    info.BasicLimitInformation.ActiveProcessLimit = limits.active_processes;
    info.ProcessMemoryLimit = limits.process_memory_bytes;
    info.JobMemoryLimit = limits.job_memory_bytes;
    unsafe {
        SetInformationJobObject(
            handle(&job),
            JobObjectExtendedLimitInformation,
            &info as *const _ as _,
            size_of_val(&info) as u32,
        )?;
        if let Some(rate) = limits.cpu_rate {
            let cpu = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION {
                ControlFlags: JOB_OBJECT_CPU_RATE_CONTROL_ENABLE
                    | JOB_OBJECT_CPU_RATE_CONTROL_HARD_CAP,
                Anonymous: JOBOBJECT_CPU_RATE_CONTROL_INFORMATION_0 { CpuRate: rate },
            };
            SetInformationJobObject(
                handle(&job),
                JobObjectCpuRateControlInformation,
                &cpu as *const _ as _,
                size_of_val(&cpu) as u32,
            )?;
        }
        let ui = JOBOBJECT_BASIC_UI_RESTRICTIONS {
            UIRestrictionsClass: JOB_OBJECT_UILIMIT(limits.ui_restrictions),
        };
        SetInformationJobObject(
            handle(&job),
            JobObjectBasicUIRestrictions,
            &ui as *const _ as _,
            size_of_val(&ui) as u32,
        )?;
    }
    Ok(job)
}
pub fn member(process: HANDLE, job: HANDLE) -> Result<bool> {
    let mut yes = BOOL::default();
    unsafe {
        IsProcessInJob(process, Some(job), &mut yes)?;
    }
    Ok(yes.as_bool())
}
pub fn terminate_job(job: HANDLE, code: u32) -> bool {
    unsafe {
        if TerminateJobObject(job, code).is_err() {
            return false;
        }
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    loop {
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        let result = unsafe {
            QueryInformationJobObject(
                Some(job),
                JobObjectBasicAccountingInformation,
                &mut info as *mut _ as _,
                size_of_val(&info) as u32,
                None,
            )
        };
        if result.is_ok() && info.ActiveProcesses == 0 {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
/// Diagnostic enumeration for `none`; job profiles use the kernel's job list.
pub fn descendants(root: u32) -> Result<Vec<u32>> {
    use windows::Win32::System::Diagnostics::ToolHelp::*;
    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0)? };
    let snapshot = unsafe { OwnedHandle::from_raw_handle(snapshot.0) };
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut entries = Vec::new();
    unsafe {
        Process32FirstW(handle(&snapshot), &mut entry)?;
        loop {
            entries.push((entry.th32ProcessID, entry.th32ParentProcessID));
            match Process32NextW(handle(&snapshot), &mut entry) {
                Ok(()) => {}
                Err(e) if e.code() == windows::core::HRESULT::from_win32(18) => break,
                Err(e) => return Err(e.into()),
            }
        }
    }
    let mut ids = std::collections::BTreeSet::from([root]);
    loop {
        let previous = ids.len();
        for &(pid, parent) in &entries {
            if ids.contains(&parent) {
                ids.insert(pid);
            }
        }
        if ids.len() == previous {
            break;
        }
    }
    Ok(ids.into_iter().collect())
}
pub fn accounting(job: HANDLE) -> Result<serde_json::Value> {
    let mut cpu = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    let mut memory = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    let mut rate = JOBOBJECT_CPU_RATE_CONTROL_INFORMATION::default();
    let mut ui = JOBOBJECT_BASIC_UI_RESTRICTIONS::default();
    unsafe {
        QueryInformationJobObject(
            Some(job),
            JobObjectCpuRateControlInformation,
            &mut rate as *mut _ as _,
            size_of_val(&rate) as u32,
            None,
        )?;
        QueryInformationJobObject(
            Some(job),
            JobObjectBasicUIRestrictions,
            &mut ui as *mut _ as _,
            size_of_val(&ui) as u32,
            None,
        )?;
        QueryInformationJobObject(
            Some(job),
            JobObjectBasicAccountingInformation,
            &mut cpu as *mut _ as _,
            size_of_val(&cpu) as u32,
            None,
        )?;
        QueryInformationJobObject(
            Some(job),
            JobObjectExtendedLimitInformation,
            &mut memory as *mut _ as _,
            size_of_val(&memory) as u32,
            None,
        )?;
    }
    Ok(
        serde_json::json!({"active_processes":cpu.ActiveProcesses,"total_processes":cpu.TotalProcesses,
        "terminated_processes":cpu.TotalTerminatedProcesses,"peak_process_memory_bytes":memory.PeakProcessMemoryUsed,
        "peak_job_memory_bytes":memory.PeakJobMemoryUsed,"cpu_seconds":(cpu.TotalUserTime+cpu.TotalKernelTime) as f64/10_000_000.,
        "limits":{"active_processes":memory.BasicLimitInformation.ActiveProcessLimit,"process_memory_bytes":memory.ProcessMemoryLimit,
            "job_memory_bytes":memory.JobMemoryLimit,"flags":memory.BasicLimitInformation.LimitFlags.0,
            "ui_restrictions":ui.UIRestrictionsClass.0,"cpu_control_flags":rate.ControlFlags.0,"cpu_rate":unsafe { rate.Anonymous.CpuRate }}}),
    )
}
/// Enumerate the job's actual process list, including nested jobs, then verify each handle.
pub fn processes(job: HANDLE) -> Result<Vec<u32>> {
    let mut storage = vec![0usize; 4098];
    unsafe {
        QueryInformationJobObject(
            Some(job),
            JobObjectBasicProcessIdList,
            storage.as_mut_ptr().cast(),
            (storage.len() * size_of::<usize>()) as u32,
            None,
        )?;
        let list = &*(storage.as_ptr() as *const JOBOBJECT_BASIC_PROCESS_ID_LIST);
        let ids = std::slice::from_raw_parts(
            list.ProcessIdList.as_ptr(),
            list.NumberOfProcessIdsInList as usize,
        );
        let mut result = Vec::new();
        for &id in ids {
            let process = match OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, id as u32) {
                Ok(h) => h,
                Err(e) if e.code() == windows::core::HRESULT::from_win32(87) => continue,
                Err(e) => return Err(e.into()),
            };
            let process = OwnedHandle::from_raw_handle(process.0);
            let mut exit = 0;
            GetExitCodeProcess(handle(&process), &mut exit)?;
            if exit != 259 {
                continue;
            }
            ensure!(
                member(handle(&process), job)?,
                "process {id} escaped instance job"
            );
            result.push(id as u32);
        }
        Ok(result)
    }
}
pub fn token_evidence(pid: u32) -> Result<serde_json::Value> {
    use super::state::{sid_string, token_info};
    use windows::Win32::Security::*;
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid)? };
    let process = unsafe { OwnedHandle::from_raw_handle(process.0) };
    let mut raw = HANDLE::default();
    unsafe {
        OpenProcessToken(handle(&process), TOKEN_QUERY, &mut raw)?;
    }
    let token = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let info = |class| token_info(handle(&token), class);
    let ac = info(TokenIsAppContainer)?[0] as u32 != 0;
    let package = if ac {
        let data = info(TokenAppContainerSid)?;
        Some(sid_string(unsafe {
            (*(data.as_ptr() as *const TOKEN_APPCONTAINER_INFORMATION)).TokenAppContainer
        })?)
    } else {
        None
    };
    let integrity = info(TokenIntegrityLevel)?;
    let integrity = sid_string(unsafe {
        (*(integrity.as_ptr() as *const TOKEN_MANDATORY_LABEL))
            .Label
            .Sid
    })?;
    let groups = |class| -> Result<Vec<serde_json::Value>> {
        let data = info(class)?;
        let group = unsafe { &*(data.as_ptr() as *const TOKEN_GROUPS) };
        let entries =
            unsafe { std::slice::from_raw_parts(group.Groups.as_ptr(), group.GroupCount as usize) };
        entries
            .iter()
            .map(|g| Ok(serde_json::json!({"sid":sid_string(g.Sid)?,"attributes":g.Attributes})))
            .collect()
    };
    let privileges = info(TokenPrivileges)?;
    let privileges = unsafe { &*(privileges.as_ptr() as *const TOKEN_PRIVILEGES) };
    let entries = unsafe {
        std::slice::from_raw_parts(
            privileges.Privileges.as_ptr(),
            privileges.PrivilegeCount as usize,
        )
    };
    let privileges: Vec<_> = entries.iter().map(|p| serde_json::json!({"luid_low":p.Luid.LowPart,"luid_high":p.Luid.HighPart,"attributes":p.Attributes.0})).collect();
    let mut mitigations = serde_json::Map::new();
    for (name, policy) in [
        ("dynamic_code", ProcessDynamicCodePolicy),
        ("win32k", ProcessSystemCallDisablePolicy),
        ("extension_points", ProcessExtensionPointDisablePolicy),
        ("signature", ProcessSignaturePolicy),
        ("aslr", ProcessASLRPolicy),
    ] {
        let mut flags = 0u32;
        let result = unsafe {
            GetProcessMitigationPolicy(handle(&process), policy, (&mut flags as *mut u32).cast(), 4)
        };
        mitigations.insert(name.into(),serde_json::json!({"flags":if result.is_ok(){Some(flags)}else{None},"error":result.err().map(|e|e.code().0)}));
    }
    let mut lpac = 0u32;
    let mut lpac_size = 0;
    let lpac_result = unsafe {
        GetTokenInformation(
            handle(&token),
            TokenIsLessPrivilegedAppContainer,
            Some((&mut lpac as *mut u32).cast()),
            4,
            &mut lpac_size,
        )
    };
    Ok(
        serde_json::json!({"pid":pid,"appcontainer":ac,"package_sid":package,"integrity":integrity,
        "capabilities":groups(TokenCapabilities)?,"restricted_sids":groups(TokenRestrictedSids)?,
        "privileges":privileges,"mitigations":mitigations,"lpac":if lpac_result.is_ok(){Some(lpac!=0)}else{None},"lpac_query_error":lpac_result.err().map(|e|e.code().0)}),
    )
}
/// Run negative access checks on this dedicated acceptance thread under A's token.
/// No broker operation or asynchronous work may execute until the guard reverts.
pub fn cross_tenant_probe(
    source: u32,
    files: &[std::path::PathBuf],
    targets: &[u32],
) -> Result<serde_json::Value> {
    use windows::Win32::Security::*;
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, source)? };
    let process = unsafe { OwnedHandle::from_raw_handle(process.0) };
    let mut raw = HANDLE::default();
    unsafe {
        OpenProcessToken(handle(&process), TOKEN_QUERY | TOKEN_DUPLICATE, &mut raw)?;
    }
    let primary = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    let mut raw = HANDLE::default();
    unsafe {
        DuplicateTokenEx(
            handle(&primary),
            TOKEN_QUERY | TOKEN_IMPERSONATE,
            None,
            SecurityImpersonation,
            TokenImpersonation,
            &mut raw,
        )?;
    }
    let impersonation = unsafe { OwnedHandle::from_raw_handle(raw.0) };
    unsafe {
        ImpersonateLoggedOnUser(handle(&impersonation))?;
    }
    struct Revert;
    impl Drop for Revert {
        fn drop(&mut self) {
            unsafe {
                if RevertToSelf().is_err() {
                    std::process::abort();
                }
            }
        }
    }
    let _revert = Revert;
    let files: Vec<_> = files.iter().map(|path| {
        let result = std::fs::read(path);
        serde_json::json!({"path":path,"allowed":result.is_ok(),"error":result.err().and_then(|e|e.raw_os_error())})
    }).collect();
    let processes: Vec<_> = targets
        .iter()
        .map(|pid| {
            let result = unsafe {
                OpenProcess(
                    PROCESS_DUP_HANDLE | PROCESS_VM_READ | PROCESS_QUERY_INFORMATION,
                    false,
                    *pid,
                )
            };
            let error = match result {
                Ok(h) => {
                    drop(unsafe { OwnedHandle::from_raw_handle(h.0) });
                    None
                }
                Err(e) => Some(e.code().0),
            };
            serde_json::json!({"pid":pid,"allowed":error.is_none(),"error":error})
        })
        .collect();
    Ok(serde_json::json!({"source_pid":source,"files":files,"processes":processes}))
}
