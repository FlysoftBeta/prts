//! Opt-in raw activation probe: no D3D manager, media types, or encoder wrapper.
#![cfg(windows)]
use anyhow::Result;
use windows::{
    Win32::{
        Media::MediaFoundation::*,
        System::{Com::*, Threading::*},
    },
    core::Interface,
};

#[test]
#[ignore = "diagnostic: requires Intel hardware; PRTS_MF_PROBE=activate|cocreate"]
fn raw_activation_lifetime() -> Result<()> {
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
        let input = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_NV12,
        };
        let output = MFT_REGISTER_TYPE_INFO {
            guidMajorType: MFMediaType_Video,
            guidSubtype: MFVideoFormat_H264,
        };
        let (mut ptr, mut count) = (std::ptr::null_mut(), 0);
        MFTEnumEx(
            MFT_CATEGORY_VIDEO_ENCODER,
            MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
            Some(&input),
            Some(&output),
            &mut ptr,
            &mut count,
        )?;
        let mut candidates = Vec::new();
        for i in 0..count as usize {
            if let Some(a) = (*ptr.add(i)).take() {
                candidates.push(a);
            }
        }
        CoTaskMemFree(Some(ptr.cast()));
        let activate = candidates
            .first()
            .ok_or_else(|| anyhow::anyhow!("no hardware MFT"))?;
        let clsid = activate.GetGUID(&MFT_TRANSFORM_CLSID_Attribute)?;
        eprintln!("CLSID={clsid:?}");
        let mode = std::env::var("PRTS_MF_PROBE").unwrap_or_else(|_| "activate".into());
        for cycle in 0..20 {
            let mft: IMFTransform = if mode == "cocreate" {
                CoCreateInstance(&clsid, None, CLSCTX_INPROC_SERVER)?
            } else {
                activate.ActivateObject()?
            };
            if let Ok(shutdown) = mft.cast::<IMFShutdown>() {
                shutdown.Shutdown()?;
            }
            drop(mft);
            if mode != "cocreate" {
                activate.ShutdownObject()?;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
            let mut handles = 0;
            GetProcessHandleCount(GetCurrentProcess(), &mut handles)?;
            eprintln!("{mode} cycle={cycle} handles={handles}");
        }
        drop(candidates);
        MFShutdown()?;
        CoUninitialize();
    }
    Ok(())
}
