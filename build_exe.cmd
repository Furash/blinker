@echo off
rem Build BlinkerUI in --onedir mode for fast boot. Output: dist\BlinkerUI\BlinkerUI.exe
rem All bundling/exclusion rules live in BlinkerUI.spec.
rem
rem After PyInstaller, force-UPX the CFG-protected Qt/Python DLLs that
rem PyInstaller refuses to compress on its own. Verified to run correctly.
setlocal enabledelayedexpansion
cd /d "%~dp0"

set "UPX_VERSION=5.0.2"
set "UPX_DIR=%~dp0tools\upx"
set "UPX_EXE=%UPX_DIR%\upx.exe"

if not exist "%UPX_EXE%" (
    echo [build] UPX not found, downloading v%UPX_VERSION%...
    if not exist "%UPX_DIR%" mkdir "%UPX_DIR%" >nul 2>&1
    set "UPX_ZIP=%TEMP%\upx-%UPX_VERSION%-win64.zip"
    set "UPX_URL=https://github.com/upx/upx/releases/download/v%UPX_VERSION%/upx-%UPX_VERSION%-win64.zip"
    powershell -NoProfile -Command "$ProgressPreference='SilentlyContinue'; Invoke-WebRequest -Uri '!UPX_URL!' -OutFile '!UPX_ZIP!'"
    if errorlevel 1 goto :upx_fail
    powershell -NoProfile -Command "$ProgressPreference='SilentlyContinue'; Expand-Archive -Path '!UPX_ZIP!' -DestinationPath '%UPX_DIR%' -Force"
    if errorlevel 1 goto :upx_fail
    rem Flatten the upx-X.Y.Z-win64\ subdir.
    for /d %%D in ("%UPX_DIR%\upx-*-win64") do (
        copy /y "%%D\upx.exe" "%UPX_EXE%" >nul
        rd /s /q "%%D"
    )
    del /q "!UPX_ZIP!" >nul 2>&1
)

if not exist "%UPX_EXE%" goto :upx_fail

echo [build] using UPX at %UPX_EXE%
python -m PyInstaller --noconfirm --clean --upx-dir "%UPX_DIR%" BlinkerUI.spec
if errorlevel 1 endlocal & exit /b 1

rem Force-UPX only the big DLLs that have been verified to run after compression.
rem Qt plugins (qwindows.dll, qmodernwindowsstyle.dll) and MSVCP140.dll do NOT
rem survive UPX -- they fail at platform-plugin load time. Don't add them back.
echo [build] post-build force-UPX on CFG-protected DLLs
set "DIST=%~dp0dist\BlinkerUI\_internal"
call :force_upx "%DIST%\python311.dll"
call :force_upx "%DIST%\PySide6\Qt6Core.dll"
call :force_upx "%DIST%\PySide6\Qt6Gui.dll"
call :force_upx "%DIST%\PySide6\Qt6Widgets.dll"
call :force_upx "%DIST%\PySide6\Qt6Svg.dll"
endlocal
exit /b 0

:force_upx
if exist "%~1" (
    "%UPX_EXE%" --force --lzma --best -q "%~1" >nul 2>&1
)
exit /b 0

:upx_fail
echo [build] WARNING: UPX unavailable, building uncompressed.
python -m PyInstaller --noconfirm --clean BlinkerUI.spec
endlocal
exit /b %ERRORLEVEL%
