//! Process setup shared by the executables (`var`, `var-check`): the bundled
//! GStreamer runtime of an installation, and no Windows error dialogs.

/// With a GStreamer runtime installed next to the executable (Windows MSI: the DLLs
/// and the plugin scanner next to `var.exe`, plugins in `lib\gstreamer-1.0`), uses only
/// that runtime: its plugins, its plugin scanner and an own plugin registry, so a
/// GStreamer installed elsewhere on the machine (e.g. by capture card software) can't
/// interfere. Does nothing without a bundled runtime (development builds).
///
/// Call first thing in `main`, before any other thread exists.
pub fn use_bundled_gstreamer() {
    let Some(dir) = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.to_owned()))
    else {
        return;
    };
    let plugins = dir.join("lib").join("gstreamer-1.0");
    if !plugins.is_dir() {
        return;
    }
    // The plugin scanner next to the executable, so Windows loads its DLLs from there;
    // in a subfolder it could pick up the DLLs of another GStreamer from PATH.
    let scanner_name = if cfg!(windows) {
        "gst-plugin-scanner.exe"
    } else {
        "gst-plugin-scanner"
    };
    let scanner = [
        dir.join(scanner_name),
        dir.join("libexec").join("gstreamer-1.0").join(scanner_name),
    ]
    .into_iter()
    .find(|path| path.is_file());
    let registry = crate::paths::log_dir()
        .parent()
        .map(|d| d.join("gstreamer-registry.bin"));
    // The bundled DLLs first, also for DLLs that plugins load at run time.
    let path = std::env::var_os("PATH").unwrap_or_default();
    let path =
        std::env::join_paths(std::iter::once(dir.clone()).chain(std::env::split_paths(&path))).ok();

    // SAFETY: called first thing in main, before any other thread exists.
    unsafe {
        std::env::set_var("GST_PLUGIN_SYSTEM_PATH_1_0", &plugins);
        std::env::remove_var("GST_PLUGIN_PATH_1_0");
        std::env::remove_var("GST_PLUGIN_PATH");
        if let Some(scanner) = scanner {
            std::env::set_var("GST_PLUGIN_SCANNER_1_0", scanner);
        }
        if let Some(registry) = registry {
            std::env::set_var("GST_REGISTRY_1_0", registry);
        }
        if let Some(path) = path {
            std::env::set_var("PATH", path);
        }
    }
}

/// No Windows error dialogs ("System Error", "Bad Image", missing DLL) for this
/// process and the processes it starts (the GStreamer plugin scanner): a modal dialog
/// must not block the operator during an event. Such errors are logged instead.
#[cfg(windows)]
pub fn suppress_error_dialogs() {
    use windows_sys::Win32::System::Diagnostics::Debug::{
        SEM_FAILCRITICALERRORS, SEM_NOOPENFILEERRORBOX, SetErrorMode,
    };
    // SAFETY: plain flags; the mode is inherited by child processes.
    unsafe {
        SetErrorMode(SEM_FAILCRITICALERRORS | SEM_NOOPENFILEERRORBOX);
    }
}

#[cfg(not(windows))]
pub fn suppress_error_dialogs() {}
