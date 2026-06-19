fn main() {
    // The `loopback` feature pulls ScreenCaptureKit (via `screencapturekit`), which
    // embeds Swift and depends on the OS Swift runtime (`libswift_Concurrency`,
    // etc.). Those live in the macOS dyld shared cache under `/usr/lib/swift`; add
    // it to the rpath so the binary (and test harness) load. Without this the
    // process aborts at launch with "Library not loaded: libswift_Concurrency".
    if std::env::var_os("CARGO_FEATURE_LOOPBACK").is_some()
        && std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos")
    {
        println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    }
    tauri_build::build()
}
