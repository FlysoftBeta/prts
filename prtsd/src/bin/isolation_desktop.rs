//! Fixed-purpose trusted provisioner. Creates no processes; accepts no tenant commands.
use anyhow::{Context, Result, ensure};
use std::{
    io::{Read, Write},
    os::windows::io::FromRawHandle,
};
use windows::{
    Win32::{
        Foundation::{HANDLE, HLOCAL, LocalFree},
        Security::{Authorization::*, *},
        System::StationsAndDesktops::*,
    },
    core::PCWSTR,
};
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn arg(prefix: &str) -> Result<String> {
    std::env::args()
        .find_map(|a| a.strip_prefix(prefix).map(str::to_owned))
        .context("missing provisioner argument")
}
fn main() -> Result<()> {
    let name = arg("--prts-desktop=")?;
    ensure!(
        name.starts_with("prts.")
            && name.len() == 41
            && name[5..].chars().all(|c| c.is_ascii_hexdigit() || c == '-'),
        "invalid desktop identity"
    );
    let package = arg("--prts-package=")?;
    ensure!(
        package.starts_with("S-1-15-2-")
            && package
                .chars()
                .all(|c| c.is_ascii_digit() || c == 'S' || c == '-'),
        "invalid package SID"
    );
    let pipes = arg("--prts-pipe=")?;
    let pipe = |label: &str| -> Result<std::fs::File> {
        let value = pipes
            .split(',')
            .find_map(|s| s.strip_prefix(label))
            .context("missing pipe")?;
        let value = usize::from_str_radix(value, 16)?;
        Ok(unsafe { std::fs::File::from_raw_handle(value as _) })
    };
    let mut command = pipe("cmd:0x")?;
    let mut event = pipe("evt:0x")?;
    let user = prtsd::isolation::state::current_user()?;
    // Only this package can enter. Restricted child tokens retain read/object rights,
    // while Chromium's original restricted-desktop deny mask remains in force.
    let sddl = wide(&format!(
        "D:P(D;;0xd013e;;;RC)(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;{user})(A;;GA;;;{package})(A;;GRGWGX;;;RC)"
    ));
    let mut sd = PSECURITY_DESCRIPTOR::default();
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(sddl.as_ptr()),
            1,
            &mut sd,
            None,
        )?;
    }
    let security = SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: sd.0,
        bInheritHandle: false.into(),
    };
    eprintln!(
        "desktop provisioner token={}",
        prtsd::isolation::win::token_evidence(std::process::id())?
    );
    unsafe {
        let desktop_name = wide(&format!("{name}.chromium_sandbox"));
        let desktop = CreateDesktopW(
            PCWSTR(desktop_name.as_ptr()),
            PCWSTR::null(),
            None,
            DESKTOP_CONTROL_FLAGS(0),
            0x000e0003,
            Some(&security),
        )
        .context("CreateDesktop")?;
        LocalFree(Some(HLOCAL(sd.0)));
        let label = wide("S:(ML;;NW;;;S-1-16-0)");
        let mut label_sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(label.as_ptr()),
            1,
            &mut label_sd,
            None,
        )?;
        let label_result =
            SetUserObjectSecurity(HANDLE(desktop.0), &LABEL_SECURITY_INFORMATION, label_sd);
        LocalFree(Some(HLOCAL(label_sd.0)));
        label_result.context("sandbox desktop untrusted label")?;
        let browser_sddl = wide(&format!(
            "D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GA;;;{user})(A;;0x200ff;;;{package})(A;;GRGWGX;;;RC)S:(ML;;NW;;;LW)"
        ));
        let mut browser_sd = PSECURITY_DESCRIPTOR::default();
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            PCWSTR(browser_sddl.as_ptr()),
            1,
            &mut browser_sd,
            None,
        )?;
        let browser_security = SECURITY_ATTRIBUTES {
            nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
            lpSecurityDescriptor: browser_sd.0,
            bInheritHandle: false.into(),
        };
        let browser_name = wide(&format!("{name}.browser"));
        let browser_desktop = CreateDesktopW(
            PCWSTR(browser_name.as_ptr()),
            PCWSTR::null(),
            None,
            DESKTOP_CONTROL_FLAGS(0),
            0x000e0003,
            Some(&browser_security),
        )
        .context("private browser desktop")?;
        LocalFree(Some(HLOCAL(browser_sd.0)));
        let mut payload = Vec::new();
        prtsd::browser_wire::string(&format!("{name}.chromium_sandbox"), &mut payload)?;
        prtsd::browser_wire::string(&format!("{name}.browser"), &mut payload)?;
        prtsd::browser_wire::Record {
            kind: 5,
            id: 1,
            payload,
        }
        .write(&mut event)?;
        event.flush()?;
        drop(event);
        let mut byte = [0u8; 1];
        // EOF is the only accepted message. No paths, tokens, handles or launch requests.
        let read = command.read(&mut byte)?;
        ensure!(read == 0, "unexpected provisioner command");
        CloseDesktop(desktop)?;
        CloseDesktop(browser_desktop)?;
    }
    Ok(())
}
