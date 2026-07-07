fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    // Embed the icon and version info into the .exe (Windows targets only).
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "BLF Decoder");
        res.set(
            "FileDescription",
            "BLF Decoder - CAN BLF+DBC to CSV/Parquet",
        );
        res.compile().expect("failed to embed Windows resources");
    }
}
