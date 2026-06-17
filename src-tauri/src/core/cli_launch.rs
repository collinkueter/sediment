//! Cross-platform CLI launching helpers (ADR-0008 / ADR-0012).
//!
//! macOS and Linux spawn the resolved binary directly. Windows needs two
//! accommodations, encapsulated here so each engine doesn't re-derive them:
//!
//! 1. **npm-global CLIs install as a `.cmd` shim** (no `.exe`) plus an
//!    extensionless bash script. `CreateProcess` cannot launch those directly —
//!    it returns `%1 is not a valid Win32 application` (os error 193). So a
//!    non-`.exe` binary is run through `cmd /C`.
//! 2. **No login-shell `PATH`.** The binary is found via Windows `where` and the
//!    `%APPDATA%\npm` npm-global directory, not `$SHELL -lc "command -v …"`.
//!
//! Every helper is a no-op (`None` / empty / direct spawn) off Windows, so call
//! sites stay branch-free.

use std::path::{Path, PathBuf};

/// npm-global candidates for `bin` under `%APPDATA%\npm` — the real `.exe` (if any)
/// preferred over the `.cmd` shim. Empty off Windows or without `%APPDATA%`.
pub fn windows_npm_candidates(bin: &str) -> Vec<PathBuf> {
    if !cfg!(windows) {
        return Vec::new();
    }
    match std::env::var("APPDATA") {
        Ok(appdata) => {
            let npm = PathBuf::from(appdata).join("npm");
            vec![
                npm.join(format!("{bin}.exe")),
                npm.join(format!("{bin}.cmd")),
            ]
        }
        Err(_) => Vec::new(),
    }
}

/// Resolve `bin` on `PATH` via Windows `where`, returning the most-launchable
/// match. `None` off Windows, when `where` fails, or when nothing is found.
pub fn where_which(bin: &str) -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let out = std::process::Command::new("where").arg(bin).output().ok()?;
    if !out.status.success() {
        return None;
    }
    most_launchable(&String::from_utf8_lossy(&out.stdout))
}

/// Whether `bin` resolves on `PATH` — `where` on Windows, `which` elsewhere.
pub fn is_on_path(bin: &str) -> bool {
    let finder = if cfg!(windows) { "where" } else { "which" };
    std::process::Command::new(finder)
        .arg(bin)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Whether `binary` must be launched through `cmd /C` — true on Windows when it is
/// not a real `.exe` (i.e. a `.cmd` / `.bat` / extensionless npm shim).
pub fn needs_cmd_shim(binary: &Path) -> bool {
    cfg!(windows)
        && binary
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| !e.eq_ignore_ascii_case("exe"))
            .unwrap_or(true)
}

/// Build a tokio [`Command`](tokio::process::Command) to launch `binary` with
/// `args`, routing a Windows `.cmd`/shim through `cmd /C`. The caller still sets
/// stdio and cwd on the returned command.
pub fn tokio_command(binary: &Path, args: &[&str]) -> tokio::process::Command {
    use tokio::process::Command;
    if needs_cmd_shim(binary) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(binary).args(args);
        c
    } else {
        let mut c = Command::new(binary);
        c.args(args);
        c
    }
}

/// Pick the most-launchable path from `where` output (one path per line): a real
/// `.exe` first, then `.cmd` / `.bat`, and the extensionless bash shim last (it is
/// the one `CreateProcess` rejects). Pure, so it is unit-tested off Windows.
fn most_launchable(where_output: &str) -> Option<PathBuf> {
    let mut paths: Vec<PathBuf> = where_output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect();
    paths.sort_by_key(|p| ext_rank(p));
    paths.into_iter().next()
}

/// Launch-preference rank by extension: `.exe` < `.cmd` < `.bat` < everything else.
fn ext_rank(p: &Path) -> u8 {
    match p
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .as_deref()
    {
        Some("exe") => 0,
        Some("cmd") => 1,
        Some("bat") => 2,
        _ => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `where` can list several matches; the launchable `.exe`/`.cmd` must win over
    /// the extensionless shim that `CreateProcess` rejects (os error 193).
    #[test]
    fn most_launchable_prefers_exe_then_cmd_over_shim() {
        let out = "C:\\Users\\x\\AppData\\Roaming\\npm\\copilot\n\
                   C:\\Users\\x\\AppData\\Roaming\\npm\\copilot.cmd\n\
                   C:\\tools\\copilot.exe\n";
        let picked = most_launchable(out).expect("a path");
        assert_eq!(picked.extension().and_then(|e| e.to_str()), Some("exe"));

        // No exe → the .cmd shim wins over the bare script.
        let out2 = "C:\\npm\\copilot\nC:\\npm\\copilot.cmd\n";
        let picked2 = most_launchable(out2).expect("a path");
        assert_eq!(picked2.extension().and_then(|e| e.to_str()), Some("cmd"));

        assert!(most_launchable("\n  \n").is_none(), "blank output → none");
    }
}
