@echo off
rem ============================================================================
rem  dev-x64-emulated.cmd  --  Run Sediment on Windows on ARM (aarch64) without a
rem  reboot, by building an x64 binary with the x64 host toolchain under
rem  emulation. See docs/windows.md ("Windows on ARM").
rem
rem  Why: on an ARM64 host, rustup defaults to the aarch64 toolchain, which needs
rem  ARM64 MSVC libs that the recommended C++ workload does NOT install. Adding
rem  them requires a reboot. This script sidesteps that by using the x64 host
rem  toolchain (Hostx64\x64 tools ship with the recommended workload).
rem
rem  Prereqs (run scripts\setup.ps1 first):
rem    - rustup x64 toolchain:  rustup toolchain install stable-x86_64-pc-windows-msvc --force-non-host
rem    - VS Build Tools (C++), NASM, CMake, Node
rem
rem  Any args are forwarded to `npm run tauri dev` (e.g. -- --release).
rem ============================================================================
setlocal

set "TOOLCHAIN=stable-x86_64-pc-windows-msvc"

rem --- Locate vcvars64.bat via vswhere ---------------------------------------
set "VSWHERE=%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe"
if not exist "%VSWHERE%" (
  echo [error] vswhere.exe not found. Install Visual Studio Build Tools ^(run scripts\setup.ps1^).
  exit /b 1
)
for /f "usebackq tokens=*" %%i in (`"%VSWHERE%" -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath`) do set "VSINSTALL=%%i"
if not defined VSINSTALL (
  echo [error] No VS install with the x64 C++ tools found. Run scripts\setup.ps1.
  exit /b 1
)
set "VCVARS=%VSINSTALL%\VC\Auxiliary\Build\vcvars64.bat"
if not exist "%VCVARS%" (
  echo [error] vcvars64.bat not found at "%VCVARS%".
  exit /b 1
)

rem --- Put build deps on PATH (idempotent if already there) -------------------
set "PATH=%USERPROFILE%\.cargo\bin;%ProgramFiles%\NASM;%ProgramFiles%\CMake\bin;%PATH%"

rem --- Enter the x64 MSVC environment so link.exe / libs resolve --------------
call "%VCVARS%" >nul
if errorlevel 1 ( echo [error] vcvars64.bat failed. & exit /b 1 )

rem --- Force the x64 toolchain (rustup won't `default` a non-host one) --------
set "RUSTUP_TOOLCHAIN=%TOOLCHAIN%"

cd /d "%~dp0.."
echo [info] Building x64 (emulated) with toolchain %TOOLCHAIN%
npm run tauri dev %*

endlocal
