//! Statically linking FFmpeg into the final Windows binary pulls in a set of
//! Win32 system libraries that the MSVC linker must be told about explicitly
//! (the FFmpeg static `.lib`s reference symbols from them but don't carry the
//! import records). These directives propagate to any binary that depends on
//! this crate, so the `cli` (and later `gui`) link picks them up.
//!
//! Harmless on dynamic / non-Windows builds: the block is gated on the Windows
//! target, and linking an unreferenced system import lib is a no-op for MSVC.

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if target_os != "windows" {
        return;
    }

    // Superset of what static FFmpeg (avcodec/avformat/avfilter/swscale/
    // swresample + zlib/dav1d) commonly needs on MSVC. Listing extras is safe;
    // omitting a needed one is a link error, so err toward completeness.
    const SYS_LIBS: &[&str] = &[
        "bcrypt",   // crypto primitives
        "ncrypt",   // CNG key storage for avformat schannel TLS (tls_schannel.o)
        "secur32",  // SSPI
        "crypt32",  // cert store
        "advapi32", // registry / legacy crypto
        "ws2_32",   // sockets
        "user32", "gdi32", "ole32", "oleaut32", "shlwapi", "strmiids", // DirectShow GUIDs
        "uuid", "mfplat", // Media Foundation (some FFmpeg paths reference it)
        "mfuuid", "vfw32", "psapi",
    ];
    for lib in SYS_LIBS {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
}
