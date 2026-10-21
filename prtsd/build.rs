fn main() {
    println!("cargo:rerun-if-changed=native/vpl.cpp");
    println!("cargo:rerun-if-changed=vendor/libvpl");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut build = cc::Build::new();
    build
        .cpp(true)
        .std("c++17")
        .warnings(false)
        .flag_if_supported("/EHsc")
        .define("MFX_DEPRECATED_OFF", None)
        .define("NOMINMAX", None)
        .include("vendor/libvpl/api")
        .include("vendor/libvpl/libvpl")
        .file("native/vpl.cpp");
    for file in [
        "windows/main.cpp",
        "windows/mfx_critical_section.cpp",
        "windows/mfx_dispatcher.cpp",
        "windows/mfx_dispatcher_log.cpp",
        "windows/mfx_driver_store_loader.cpp",
        "windows/mfx_dxva2_device.cpp",
        "windows/mfx_function_table.cpp",
        "windows/mfx_library_iterator.cpp",
        "windows/mfx_load_dll.cpp",
        "windows/mfx_win_reg_key.cpp",
        "mfx_dispatcher_vpl.cpp",
        "mfx_dispatcher_vpl_loader.cpp",
        "mfx_dispatcher_vpl_config.cpp",
        "mfx_dispatcher_vpl_lowlatency.cpp",
        "mfx_dispatcher_vpl_log.cpp",
        "mfx_dispatcher_vpl_msdk.cpp",
        "mfx_config_interface/mfx_config_interface.cpp",
        "mfx_config_interface/mfx_config_interface_string_api.cpp",
    ] {
        build.file(format!("vendor/libvpl/libvpl/src/{file}"));
    }
    build.compile("prts_onevpl");
    for library in [
        "dxgi", "d3d11", "ole32", "advapi32", "user32", "gdi32", "cfgmgr32", "version", "setupapi",
        "shlwapi",
    ] {
        println!("cargo:rustc-link-lib={library}");
    }
}
