@echo off
rem Build BlinkerUI in --onedir mode for fast boot. Output: dist\BlinkerUI\BlinkerUI.exe
rem All bundling/exclusion rules live in BlinkerUI.spec.
setlocal
cd /d "%~dp0"
python -m PyInstaller --noconfirm BlinkerUI.spec
endlocal
