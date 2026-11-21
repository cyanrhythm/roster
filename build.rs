fn main() {
    println!("cargo:rerun-if-changed=package/icon.ico");

    if cfg!(target_os = "windows") {
        let mut res = winres::WindowsResource::new();
        res.set_icon("package/icon.ico");
        res.set("ProductName", "roster");
        res.set("FileDescription", "TUI manager for agent sessions");
        res.compile().expect("failed to embed Windows resources");
    }
}
