//! Embeds the application icon and version metadata into the Windows .exe.
//! No-op on other platforms.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../assets/icon.ico");

    // Must test the TARGET, not the host: build scripts compile for the host, so
    // `cfg!(target_os)` would be true on a macOS build and false when
    // cross-compiling to Windows — exactly backwards.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/icon.ico");
        res.compile().expect("failed to embed Windows resources");
    }
}
