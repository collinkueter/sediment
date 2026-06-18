# Running Sediment on Windows

Sediment runs on Windows 10 and 11 alongside macOS. The conversational engines
(Claude Code, Copilot), the Ollama sidecar, the on-device embedder, and the
file watcher are all cross-platform; the Windows-specific accommodations are
documented here. macOS users do not need any of this.

## Prerequisites

| Requirement | Notes |
| --- | --- |
| **Rust ≥ 1.88 (MSVC)** | Install via [rustup](https://rustup.rs/). The default `x86_64-pc-windows-msvc` toolchain is correct — do **not** use the GNU toolchain. |
| **Visual Studio Build Tools** | Install "Build Tools for Visual Studio" and select the **Desktop development with C++** workload. This provides the MSVC linker (`link.exe`) and the Windows SDK that the native Rust dependencies (SurrealDB, `ort`/ONNX Runtime, Tauri) link against. |
| **WebView2 runtime** | Preinstalled on Windows 11. On Windows 10 the packaged installer provisions it automatically (`webviewInstallMode: downloadBootstrapper` in `tauri.conf.json`); for a `tauri dev` session on a bare Windows 10 box, install the [Evergreen WebView2 Runtime](https://developer.microsoft.com/microsoft-edge/webview2/) once. |
| **Node ≥ 22 + npm** | Same as macOS. |
| **An agent CLI** | [Claude Code](https://claude.com/claude-code) and/or `npm install -g @github/copilot`, signed in. |
| **Ollama** *(optional)* | Only needed for the Ollama embedding option. The default on-device embedder (ADR-0014) needs nothing. |

## Build & run

From PowerShell or Windows Terminal:

```powershell
git clone <repo> sediment; cd sediment
npm install
npm run tauri dev
```

The first build is slow — the Rust dep tree plus a one-time `ort` download of the
ONNX Runtime for the on-device embedder. Subsequent builds are incremental.

To produce installers (`.msi` and NSIS `.exe`):

```powershell
npm run tauri build
```

## How the Windows accommodations work

### Agent-CLI discovery and launch (`core::cli_launch`)

npm-global CLIs install on Windows as a `.cmd` shim (plus an extensionless bash
script) under `%APPDATA%\npm`, with no `.exe`. Two problems follow, both handled
centrally in [`src-tauri/src/core/cli_launch.rs`](../src-tauri/src/core/cli_launch.rs):

1. **`CreateProcess` cannot launch a `.cmd` directly** — it returns *"%1 is not a
   valid Win32 application"* (os error 193). So any non-`.exe` binary is run
   through `cmd /C` (`tokio_command`).
2. **A GUI app does not inherit a login-shell `PATH`.** The binary is resolved
   via `%APPDATA%\npm` candidates and the Windows `where` command (`where_which`),
   not a `$SHELL -lc "command -v …"` probe.

`claude_code::locate`, `copilot::locate`, and `ollama_sidecar` all route through
this module. Ollama installs a native `ollama.exe` on PATH, so its daemon spawn
(`ollama serve`) runs directly once `where ollama` finds it.

### On-device embedder cache

`core::bundled_embed::cache_dir` resolves `USERPROFILE` when `HOME` is unset
(the usual Windows case), so the ONNX weights land in
`%USERPROFILE%\.sediment\fastembed` and are shared between the main process and
the `--mcp-stdio` subprocess.

### Window chrome

The window is **frameless** on both desktop platforms but the chrome differs:

- **macOS** overlays the native traffic lights (`titleBarStyle: Overlay`), so the
  title bar reserves a 78px safe area on the left.
- **Windows** is fully undecorated (`decorations: false` in
  [`tauri.windows.conf.json`](../src-tauri/tauri.windows.conf.json)); the app
  draws its own minimize / maximize / close controls flush to the right edge.
  Platform is detected from the WebView user agent in
  [`src/lib/platform.ts`](../src/lib/platform.ts); the controls live in
  `TitleBar.tsx` and drive the `@tauri-apps/api/window` API (permitted in
  `capabilities/default.json`).

The window is resizable from its edges. Native Windows *snap-layout* flyouts
(hovering the maximize button) are not yet wired up — a future polish item.

## Known caveats

- **Reminder toasts.** Windows shows toast notifications reliably only for an
  *installed* app (it needs a Start-menu shortcut / AppUserModelID). In a
  `tauri dev` session reminders may be silent; install a built bundle to see
  them.
- **Code signing.** The MSI/NSIS bundles are unsigned by default, so SmartScreen
  will warn on first run. Signing is a separate release-time concern.
