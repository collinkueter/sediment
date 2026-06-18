<#
.SYNOPSIS
    One-shot Windows dev-environment bootstrap for Sediment.

.DESCRIPTION
    Installs the prerequisites a fresh Windows 10/11 box needs to run
    `npm run tauri dev`:

      - Node.js LTS (>= 22)              via winget (OpenJS.NodeJS.LTS)
      - Visual Studio Build Tools        via winget (Microsoft.VisualStudio.2022.BuildTools)
        with the "Desktop development with C++" workload (MSVC linker + Windows SDK);
        adds the ARM64 target tools on ARM64 hosts.
      - Rust, MSVC toolchain             via winget (Rustlang.Rustup)
      - NASM + CMake                     via winget -- required to build aws-lc-sys (rustls crypto)
      - (checks) WebView2 runtime        warns if absent (preinstalled on Win 11)

    Then runs `npm install`. Every step is idempotent: anything already present
    is detected and skipped, so the script is safe to re-run.

    It does NOT install an agent CLI (Claude Code / GitHub Copilot) or Ollama --
    those are per-developer choices. See the end-of-run summary and docs/windows.md.

.PARAMETER SkipNpmInstall
    Skip the final `npm install` step (toolchain only).

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File scripts\setup.ps1
#>
[CmdletBinding()]
param(
    [switch]$SkipNpmInstall
)

$ErrorActionPreference = "Stop"

function Write-Step    ($m) { Write-Host "`n=== $m ===" -ForegroundColor Cyan }
function Write-Ok      ($m) { Write-Host "  [ok]   $m" -ForegroundColor Green }
function Write-Warn2   ($m) { Write-Host "  [warn] $m" -ForegroundColor Yellow }
function Write-Action  ($m) { Write-Host "  [..]   $m" -ForegroundColor Gray }

# winget exits non-zero (e.g. 0x8A150061 "already installed") in cases that are
# fine for us; treat the "no applicable upgrade / already installed" codes as ok.
function Invoke-Winget {
    param([string]$Id, [string[]]$ExtraArgs = @())
    $wingetArgs = @(
        "install", "--id", $Id, "-e",
        "--accept-source-agreements", "--accept-package-agreements",
        "--disable-interactivity"
    ) + $ExtraArgs
    Write-Action "winget install $Id"
    & winget @wingetArgs
    $code = $LASTEXITCODE
    # 0 = installed; -1978335189 (0x8A15002B) = no upgrade found / already installed
    if ($code -eq 0 -or $code -eq -1978335189) { return $true }
    Write-Warn2 "winget returned exit code $code for $Id (continuing -- verify manually if a later step fails)"
    return $false
}

if (-not (Get-Command winget -ErrorAction SilentlyContinue)) {
    throw "winget is not available. Install 'App Installer' from the Microsoft Store, then re-run this script."
}

# --- Node.js -----------------------------------------------------------------
Write-Step "Node.js (>= 22)"
$node = Get-Command node -ErrorAction SilentlyContinue
if ($node) {
    $ver = (& node --version) -replace '^v',''
    if ([version]($ver.Split('-')[0]) -ge [version]"22.0.0") {
        Write-Ok "node $ver already installed"
    } else {
        Write-Warn2 "node $ver is older than 22 -- installing LTS"
        Invoke-Winget "OpenJS.NodeJS.LTS" | Out-Null
    }
} else {
    Invoke-Winget "OpenJS.NodeJS.LTS" | Out-Null
}

# --- Visual Studio Build Tools (C++) -----------------------------------------
Write-Step "Visual Studio Build Tools (Desktop development with C++)"
# On ARM64 Windows, rustup defaults to the aarch64 toolchain, which links against
# the ARM64 MSVC libs -- an extra component the "Desktop development with C++"
# workload's recommended set does NOT include. Add it on ARM64 hosts.
$isArm64 = $env:PROCESSOR_ARCHITECTURE -eq "ARM64"
$vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
$haveX64 = $false
$haveArm64 = $false
if (Test-Path $vswhere) {
    $vc = & $vswhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
    if ($vc) { $haveX64 = $true }
    $arm = & $vswhere -products * -requires Microsoft.VisualStudio.Component.VC.Tools.ARM64 -property installationPath
    if ($arm) { $haveArm64 = $true }
}
$needArm64Comp = $isArm64 -and -not $haveArm64
if ($haveX64 -and -not $needArm64Comp) {
    Write-Ok "MSVC C++ build tools already present ($vc)"
} else {
    $components = @("--add", "Microsoft.VisualStudio.Workload.VCTools", "--includeRecommended")
    if ($isArm64) { $components += @("--add", "Microsoft.VisualStudio.Component.VC.Tools.ARM64") }
    Write-Action "Installing Build Tools with the VCTools workload$(if($isArm64){' + ARM64 target tools'}) (multi-GB; the slow step)"
    Invoke-Winget "Microsoft.VisualStudio.2022.BuildTools" @(
        "--override",
        ("--quiet --wait --norestart " + ($components -join " "))
    ) | Out-Null
    Write-Warn2 "The Build Tools installer may request a reboot to finalize. If a later 'cargo' build fails to find the linker, reboot and re-run this script."
}

# --- Rust (MSVC toolchain) ---------------------------------------------------
Write-Step "Rust (rustup, MSVC toolchain)"
if (Get-Command cargo -ErrorAction SilentlyContinue) {
    Write-Ok "cargo $((& cargo --version)) already installed"
} elseif (Test-Path "$env:USERPROFILE\.cargo\bin\cargo.exe") {
    Write-Ok "cargo present at ~\.cargo\bin (restart your shell to pick it up on PATH)"
} else {
    Invoke-Winget "Rustlang.Rustup" | Out-Null
    # rustup auto-selects the host toolchain (x86_64 on Intel/AMD, aarch64 on ARM64).
    # Don't force x86_64 here -- that fails on an ARM64 host.
    Write-Ok "rustup installed (default host toolchain selected automatically)"
}

# --- Native build deps: NASM + CMake -----------------------------------------
# aws-lc-sys (pulled in transitively via rustls) assembles its crypto with NASM
# and drives its build with CMake on Windows. Without these, `cargo build` panics
# with "NASM command not found!".
Write-Step "Native build deps (NASM, CMake)"
if (Get-Command nasm -ErrorAction SilentlyContinue) {
    Write-Ok "nasm already installed"
} else {
    Invoke-Winget "NASM.NASM" | Out-Null
    # The NASM package does not always add itself to PATH; ensure it is there.
    $nasmDir = "$env:ProgramFiles\NASM"
    if (Test-Path "$nasmDir\nasm.exe") {
        $userPath = [Environment]::GetEnvironmentVariable('Path','User')
        if ($userPath -notlike "*$nasmDir*") {
            [Environment]::SetEnvironmentVariable('Path', ($userPath.TrimEnd(';') + ";$nasmDir"), 'User')
            Write-Ok "Added $nasmDir to your user PATH (effective in new shells)"
        }
    }
}
if (Get-Command cmake -ErrorAction SilentlyContinue) {
    Write-Ok "cmake already installed"
} else {
    Invoke-Winget "Kitware.CMake" | Out-Null
}

# --- WebView2 (check only) ---------------------------------------------------
Write-Step "WebView2 runtime"
$wvKeys = @(
    'HKLM:\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
    'HKCU:\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
)
$wv = $wvKeys | ForEach-Object { Get-ItemProperty -Path $_ -ErrorAction SilentlyContinue } | Where-Object { $_.pv } | Select-Object -First 1
if ($wv) {
    Write-Ok "WebView2 runtime $($wv.pv)"
} else {
    Write-Warn2 "WebView2 runtime not detected. Preinstalled on Windows 11; on Windows 10 install the Evergreen runtime: https://developer.microsoft.com/microsoft-edge/webview2/"
}

# --- npm install -------------------------------------------------------------
if (-not $SkipNpmInstall) {
    Write-Step "npm install"
    $repoRoot = Split-Path -Parent $PSScriptRoot
    Push-Location $repoRoot
    try {
        $npm = Get-Command npm -ErrorAction SilentlyContinue
        if ($npm) {
            & npm install
            Write-Ok "node_modules installed"
        } else {
            Write-Warn2 "npm not on PATH in this shell (Node was just installed). Open a NEW terminal and run 'npm install'."
        }
    } finally {
        Pop-Location
    }
}

# --- Summary -----------------------------------------------------------------
Write-Step "Next steps"
Write-Host @"
  1. Open a NEW terminal so freshly-installed tools land on PATH.
  2. Install and sign in to an agent CLI (pick one in Settings):
       - Claude Code:   https://claude.com/claude-code
       - GitHub Copilot: npm install -g @github/copilot   (then: copilot)
  3. (Optional) Ollama for the Ollama embedding option: https://ollama.com/download
  4. Launch the app:
       npm run tauri dev

  The first build compiles a large Rust dep tree and downloads the ONNX runtime
  for the on-device embedder -- expect 10-20 min cold. Subsequent builds are fast.

  Windows specifics & troubleshooting: docs/windows.md
"@ -ForegroundColor White
