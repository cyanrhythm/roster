//! Embeds the Windows resources (icon + version strings) into the executable.
//!
//! The `.ico` is checked in next to the `.svg` it is rasterised from; only the
//! `.ico` is used at build time.

use std::path::Path;

const ICON: &str = "package/icon.ico";

fn main() {
    println!("cargo:rerun-if-changed={ICON}");

    // Resources are a Windows concept; other targets just carry the binary.
    if !cfg!(target_os = "windows") {
        return;
    }
    assert!(
        Path::new(ICON).is_file(),
        "{ICON} is missing; it is checked into the repository (see README, 图标)"
    );

    // Taken from Cargo.toml so a rename cannot leave a stale product name in
    // the file-properties dialog.
    let name = env!("CARGO_PKG_NAME");
    let mut res = winres::WindowsResource::new();
    res.set_icon(ICON);
    res.set("ProductName", name);
    res.set("InternalName", name);
    res.set("OriginalFilename", &format!("{name}.exe"));
    res.set("FileDescription", env!("CARGO_PKG_DESCRIPTION"));
    res.compile().expect("failed to embed Windows resources");
}
