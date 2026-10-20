use prtsd::{
    isolation::{Config, JobLimits},
    model::Options,
};
use serde_json::json;

#[test]
fn isolation_never_downgrades_unavailable_or_unknown_requests() {
    for profile in ["appcontainer", "lpac", "unknown", "", "JOB"] {
        let result = Options::parse(&json!({"isolation_profile":profile}));
        assert!(matches!(result, Err(e) if e.code == "isolation_error"));
    }
    for profile in ["none", "job"] {
        let options = Options::parse(&json!({"isolation_profile":profile})).unwrap();
        assert_eq!(options.isolation_profile, profile);
    }
}

#[test]
fn malformed_job_limits_are_rejected_before_a_process_exists() {
    for limits in [
        json!({"cpu_rate":0}),
        json!({"cpu_rate":10001}),
        json!({"active_processes":0}),
        json!({"ui_restrictions":256}),
        json!({"process_memory_bytes":1}),
        json!({"job_memory_bytes":16*1024*1024}),
        json!({"breakaway":true}),
    ] {
        assert!(Options::parse(&json!({"isolation_profile":"job","job_limits":limits})).is_err());
    }
    let limits: JobLimits =
        serde_json::from_value(json!({"cpu_rate":5000,"ui_restrictions":128})).unwrap();
    limits.validate().unwrap();
    assert_eq!(limits.active_processes, 64);
    assert_eq!(limits.process_memory_bytes, 4usize * 1024 * 1024 * 1024);
}

#[test]
fn isolation_config_is_strict_and_defaults_to_no_ui_limits() {
    let config: Config = serde_json::from_value(json!({})).unwrap();
    config.validate().unwrap();
    assert_eq!(config.profile, "job");
    assert_eq!(config.job_limits.ui_restrictions, 0);
    assert!(config.job_limits.cpu_rate.is_none());
    assert!(serde_json::from_value::<Config>(json!({"fallback":"none"})).is_err());
}

#[cfg(windows)]
#[test]
fn job_enforces_process_quota_and_closing_its_only_handle_kills_suspended_member()
-> anyhow::Result<()> {
    use prtsd::isolation::win;
    use std::{
        mem::size_of,
        os::windows::io::{FromRawHandle, OwnedHandle},
    };
    use windows::{
        Win32::{Foundation::WAIT_OBJECT_0, System::Threading::*},
        core::{PCWSTR, PWSTR},
    };
    let limits = JobLimits {
        active_processes: 1,
        process_memory_bytes: 64 * 1024 * 1024,
        job_memory_bytes: 128 * 1024 * 1024,
        cpu_rate: Some(2500),
        ..Default::default()
    };
    let job = win::create_job(&limits)?;
    let accounting = win::accounting(win::handle(&job))?;
    assert_eq!(accounting["limits"]["active_processes"], 1);
    assert_eq!(accounting["limits"]["cpu_rate"], 2500);
    assert_eq!(accounting["limits"]["cpu_control_flags"], 5); // ENABLE | HARD_CAP
    assert_eq!(accounting["limits"]["ui_restrictions"], 0);
    assert_eq!(
        accounting["limits"]["process_memory_bytes"],
        64 * 1024 * 1024
    );
    let mut size = 0;
    unsafe {
        let _ = InitializeProcThreadAttributeList(None, 1, None, &mut size);
    }
    let mut storage = vec![0usize; size.div_ceil(size_of::<usize>())];
    let list = LPPROC_THREAD_ATTRIBUTE_LIST(storage.as_mut_ptr().cast());
    unsafe {
        InitializeProcThreadAttributeList(Some(list), 1, None, &mut size)?;
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
    let jobs = [win::handle(&job)];
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
    let exe = std::env::current_exe()?.to_string_lossy().into_owned();
    let app: Vec<_> = exe.encode_utf16().chain(Some(0)).collect();
    let mut startup = STARTUPINFOEXW::default();
    startup.StartupInfo.cb = size_of::<STARTUPINFOEXW>() as u32;
    startup.lpAttributeList = list;
    let create = || -> windows::core::Result<PROCESS_INFORMATION> {
        let mut command: Vec<_> = format!("\"{exe}\"").encode_utf16().chain(Some(0)).collect();
        let mut process = PROCESS_INFORMATION::default();
        unsafe {
            CreateProcessW(
                PCWSTR(app.as_ptr()),
                Some(PWSTR(command.as_mut_ptr())),
                None,
                None,
                false,
                EXTENDED_STARTUPINFO_PRESENT | CREATE_SUSPENDED | CREATE_NO_WINDOW,
                None,
                PCWSTR::null(),
                &startup.StartupInfo,
                &mut process,
            )?;
        }
        Ok(process)
    };
    let first = create()?;
    let process = unsafe { OwnedHandle::from_raw_handle(first.hProcess.0) };
    let thread = unsafe { OwnedHandle::from_raw_handle(first.hThread.0) };
    assert!(win::member(win::handle(&process), win::handle(&job))?);
    match create() {
        Err(_) => {}
        Ok(unexpected) => unsafe {
            let _ = TerminateProcess(unexpected.hProcess, 1);
            drop(OwnedHandle::from_raw_handle(unexpected.hThread.0));
            drop(OwnedHandle::from_raw_handle(unexpected.hProcess.0));
            panic!("job accepted a second process above its active-process limit");
        },
    }
    drop(job); // No worker ran; this tests the launcher crash-cleanup primitive itself.
    assert_eq!(
        unsafe { WaitForSingleObject(win::handle(&process), 5000) },
        WAIT_OBJECT_0
    );
    drop(thread);
    Ok(())
}
