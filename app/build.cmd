@echo off
rem Local release build + packaging for the Tauri app (same steps as CI).
rem
rem   app\build.cmd [tag]        default tag: v0.0.0-local
rem   set SKIP_TESTS=1           skip `cargo test`
rem
rem Needs rustup (app\rust-toolchain.toml pins the toolchain) and tauri-cli 2.x
rem (cargo install tauri-cli --version 2.12.4 --locked).
rem Output: app\dist\BlinkerUI-<tag>.zip and .zip.sha256.
rem The version is stamped into tauri.conf.json / Cargo.toml / Cargo.lock for the
rem build and restored afterwards (git checkout of those three files), so run
rem this with those files committed or stashed.
setlocal
cd /d "%~dp0"
set TAG=%~1
if "%TAG%"=="" set TAG=v0.0.0-local
set VERSION=%TAG:~1%

powershell -NoProfile -ExecutionPolicy Bypass -File stamp-version.ps1 -Version %VERSION% || goto :fail
if "%SKIP_TESTS%"=="1" goto :build
pushd src-tauri
cargo test --locked
rem Capture the result before popd, which would reset ERRORLEVEL.
set RC=%ERRORLEVEL%
popd
if not "%RC%"=="0" goto :fail_rc
:build
cargo tauri build --no-bundle -- --locked || goto :fail
powershell -NoProfile -ExecutionPolicy Bypass -File package.ps1 -Tag %TAG% -OutDir dist || goto :fail
git checkout -- src-tauri/tauri.conf.json src-tauri/Cargo.toml src-tauri/Cargo.lock
echo Done: app\dist\BlinkerUI-%TAG%.zip
exit /b 0

:fail
set RC=%ERRORLEVEL%
:fail_rc
git checkout -- src-tauri/tauri.conf.json src-tauri/Cargo.toml src-tauri/Cargo.lock
echo Build failed.
exit /b %RC%
