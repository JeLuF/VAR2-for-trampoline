//! Windows: embeds the application icon and version information in `var.exe`, so
//! Explorer, the Start menu, shortcuts and the taskbar show the icon.

fn main() {
    println!("cargo:rerun-if-changed=../assets/var.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("../assets/var.ico")
            .set("ProductName", "VAR for Trampoline")
            .set("FileDescription", "VAR for Trampoline")
            .set("LegalCopyright", "MIT licence");
        if let Err(e) = resource.compile() {
            // Without the Windows SDK's resource compiler the application still
            // builds, only without the icon in Explorer.
            println!("cargo:warning=application icon not embedded: {e}");
        }
    }
}
