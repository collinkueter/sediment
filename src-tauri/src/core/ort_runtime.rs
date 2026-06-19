//! ONNX Runtime provisioning for the embedder under `local-asr` (ADR-0014/0016).
//!
//! Why this exists: with `local-asr`, sherpa-onnx statically links its own newer
//! ONNX Runtime, so `ort` (the bundled embedder's runtime) is switched to
//! `load-dynamic` to avoid two static runtimes colliding in one binary (see the
//! `local-asr` feature note in `Cargo.toml`). `load-dynamic` means `ort` loads
//! `libonnxruntime` at runtime from `ORT_DYLIB_PATH` — so *something* must put a
//! matching dylib on disk and set that variable before the first embedding.
//!
//! This is that something, in the same local-only / explicit-acquisition spirit as
//! `bundled_embed` (ADR-0016): the runtime lib lives in a fixed app-owned dir and
//! is fetched once. Only the bundled embedder needs it (the Ollama / keyword
//! providers never touch `ort`), so [`ensure`] is best-effort and lazy — it sets
//! the env var if a lib is already present and downloads one only when asked.

use crate::error::{AppError, AppResult};
use std::path::{Path, PathBuf};

/// ONNX Runtime version to provision — pinned to what `ort` 2.0.0-rc.9 targets, so
/// the dynamically-loaded lib matches the ABI `ort`/`fastembed` were built against.
const ORT_VERSION: &str = "1.20.0";

fn home_dir() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
}

/// App-owned directory holding the provisioned ONNX Runtime shared library.
pub fn runtime_dir() -> PathBuf {
    home_dir().join(".sediment").join("runtime")
}

/// The shared-library filename `ort` looks for on this platform.
fn lib_filename() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        "onnxruntime.dll"
    }
    #[cfg(target_os = "macos")]
    {
        "libonnxruntime.dylib"
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        "libonnxruntime.so"
    }
}

/// The Microsoft release archive for this platform, and the path of the shared lib
/// inside it. `None` on a target we don't provision (the build won't use ORT there).
fn dist() -> Option<(String, &'static str)> {
    let base = "https://github.com/microsoft/onnxruntime/releases/download";
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    let archive = format!("onnxruntime-osx-arm64-{ORT_VERSION}.tgz");
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    let archive = format!("onnxruntime-osx-x86_64-{ORT_VERSION}.tgz");
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    let archive = format!("onnxruntime-win-x64-{ORT_VERSION}.zip");
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    let archive = format!("onnxruntime-win-arm64-{ORT_VERSION}.zip");
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    let archive = format!("onnxruntime-linux-x64-{ORT_VERSION}.tgz");
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
    )))]
    return None;

    #[cfg(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
    ))]
    Some((format!("{base}/v{ORT_VERSION}/{archive}"), lib_filename()))
}

/// The installed shared-library path, if present.
pub fn installed() -> Option<PathBuf> {
    let p = runtime_dir().join(lib_filename());
    p.is_file().then_some(p)
}

/// True once `ORT_DYLIB_PATH` points at an existing file. Honours an
/// externally-set value (a developer pointing at their own lib).
pub fn ready() -> bool {
    std::env::var("ORT_DYLIB_PATH")
        .ok()
        .map(|p| Path::new(&p).is_file())
        .unwrap_or(false)
        || installed().is_some()
}

/// Point `ort` at the installed runtime by setting `ORT_DYLIB_PATH`, unless it is
/// already set to an existing file. No-op when nothing is installed yet.
///
/// `std::env::set_var` is not thread-safe against a concurrent `getenv` elsewhere,
/// so the write is guarded to happen **at most once** per process (the first moment
/// a lib is on disk — normally the startup call, before worker threads spin up). A
/// single bounded write is the practical mitigation short of `ort`'s programmatic
/// dylib API.
pub fn set_env_if_present() {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WRITTEN: AtomicBool = AtomicBool::new(false);

    if let Ok(existing) = std::env::var("ORT_DYLIB_PATH") {
        if Path::new(&existing).is_file() {
            return;
        }
    }
    if let Some(path) = installed() {
        if WRITTEN.swap(true, Ordering::SeqCst) {
            return; // already wrote it once — don't race another getenv
        }
        std::env::set_var("ORT_DYLIB_PATH", path);
    }
}

/// Ensure a usable ONNX Runtime is on disk and `ORT_DYLIB_PATH` is set: a no-op
/// when already present, otherwise download the platform archive, extract the
/// shared library into [`runtime_dir`], and set the env var. Best-effort — returns
/// an error the caller logs; the embedder then surfaces its own actionable error.
pub async fn ensure() -> AppResult<()> {
    set_env_if_present();
    if ready() {
        return Ok(());
    }
    let Some((url, inner_lib)) = dist() else {
        return Err(AppError::other(
            "no ONNX Runtime distribution for this platform",
        ));
    };
    let dir = runtime_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| AppError::other(format!("create runtime dir: {e}")))?;

    // Download the archive to a temp file next to the target dir.
    let archive = dir.join("onnxruntime-download.tmp");
    let bytes = reqwest::get(&url)
        .await
        .map_err(|e| AppError::other(format!("download onnxruntime: {e}")))?
        .error_for_status()
        .map_err(|e| AppError::other(format!("download onnxruntime: {e}")))?
        .bytes()
        .await
        .map_err(|e| AppError::other(format!("download onnxruntime: {e}")))?;
    tokio::fs::write(&archive, &bytes)
        .await
        .map_err(|e| AppError::other(format!("write onnxruntime archive: {e}")))?;

    // Extract with the system `tar` (bsdtar handles both .tgz and .zip; present on
    // macOS, Linux, and Windows 10+), then lift the shared lib to runtime_dir.
    let dir_for_extract = dir.clone();
    let target_lib = dir.join(lib_filename());
    let inner = inner_lib.to_string();
    tokio::task::spawn_blocking(move || -> AppResult<()> {
        let status = std::process::Command::new("tar")
            .arg("-xf")
            .arg(&archive)
            .arg("-C")
            .arg(&dir_for_extract)
            .status()
            .map_err(|e| AppError::other(format!("extract onnxruntime (tar): {e}")))?;
        if !status.success() {
            return Err(AppError::other("extract onnxruntime: tar failed"));
        }
        // The release nests the lib under `<archive-stem>/lib/<inner>`; find it.
        let found = find_named(&dir_for_extract, &inner)
            .ok_or_else(|| AppError::other(format!("onnxruntime lib {inner} not in archive")))?;
        std::fs::copy(&found, &target_lib)
            .map_err(|e| AppError::other(format!("install onnxruntime lib: {e}")))?;
        let _ = std::fs::remove_file(&archive);
        Ok(())
    })
    .await
    .map_err(|e| AppError::other(format!("extract onnxruntime join: {e}")))??;

    set_env_if_present();
    if ready() {
        Ok(())
    } else {
        Err(AppError::other("onnxruntime install did not take"))
    }
}

/// Install the ONNX Runtime shared library from a user-chosen folder — the offline
/// / air-gapped path. Looks for the platform lib (`libonnxruntime.dylib` /
/// `onnxruntime.dll` / `.so`) anywhere under `src`, copies it into [`runtime_dir`],
/// and sets `ORT_DYLIB_PATH`. No-op (Ok) if a runtime is already available. Errors
/// if no matching lib is in the folder, so the caller can fall back to a download.
pub fn import_from_dir(src: &Path) -> AppResult<()> {
    set_env_if_present();
    if ready() {
        return Ok(());
    }
    let name = lib_filename();
    let found = find_named(src, name)
        .ok_or_else(|| AppError::other(format!("no {name} in the chosen folder")))?;
    let dir = runtime_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| AppError::other(format!("create runtime dir: {e}")))?;
    std::fs::copy(&found, dir.join(name))
        .map_err(|e| AppError::other(format!("install onnxruntime lib: {e}")))?;
    set_env_if_present();
    Ok(())
}

/// Recursively find a file named `name` under `root` (the lib is a few levels deep
/// in the release archive). Bounded by the small archive tree.
fn find_named(root: &Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(hit) = find_named(&path, name) {
                return Some(hit);
            }
        } else if path.file_name().and_then(|n| n.to_str()) == Some(name) {
            return Some(path);
        }
    }
    None
}
