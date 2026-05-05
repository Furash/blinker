#!/usr/bin/env python3
"""PySide6 UI for blinker. Manages addon folders, AI launchers, theme."""

from __future__ import annotations

import ctypes
import hashlib
import io
import json
import os
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
from ctypes import wintypes
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path
from urllib.parse import urlsplit

from PySide6.QtCore import (
    QProcess, QProcessEnvironment, QSize, Qt, QTimer, Signal,
)
from PySide6.QtGui import QAction, QColor, QFont, QFontDatabase, QIcon, QTextCursor
from PySide6.QtWidgets import (
    QApplication, QColorDialog, QComboBox, QDialog, QDialogButtonBox,
    QFileDialog, QFormLayout, QFrame, QGridLayout, QHBoxLayout, QLabel,
    QLineEdit, QListWidget, QListWidgetItem, QMainWindow, QMessageBox,
    QPlainTextEdit, QPushButton, QScrollArea, QSizePolicy, QSpinBox,
    QSplitter, QTabWidget, QToolBar, QVBoxLayout, QWidget,
)

# ---------- version / update endpoint ----------

__version__ = "0.1.0"
GITHUB_REPO = "Furash/blinker"
UPDATE_API = f"https://api.github.com/repos/{GITHUB_REPO}/releases/latest"


# ---------- paths (handles PyInstaller --onefile) ----------

FROZEN = getattr(sys, "frozen", False)
BUNDLE_DIR = Path(getattr(sys, "_MEIPASS", "")) if FROZEN else Path(__file__).resolve().parent
APP_DIR = Path(sys.executable).resolve().parent if FROZEN else Path(__file__).resolve().parent
BOOTSTRAP = BUNDLE_DIR / "bootstrap.py"
LOGO = BUNDLE_DIR / "logo.png"
ICONS_DIRS = (APP_DIR / "icons", BUNDLE_DIR / "icons")
CONFIG = APP_DIR / "blinker_ui_config.json"

_ICON_CACHE: dict[str, "QIcon | None"] = {}


def find_alias_icon(alias: str):
    """Look up icon for an AI alias. Searches APP_DIR/icons then BUNDLE_DIR/icons.
    Accepts .svg and .png. Returns QIcon or None."""
    name = alias.strip().lower()
    if not name:
        return None
    if name in _ICON_CACHE:
        return _ICON_CACHE[name]
    from PySide6.QtGui import QIcon as _QIcon
    for d in ICONS_DIRS:
        for ext in ("svg", "png"):
            p = d / f"{name}.{ext}"
            if p.is_file():
                icon = _QIcon(str(p))
                _ICON_CACHE[name] = icon
                return icon
    _ICON_CACHE[name] = None
    return None

PORT_BASE = 9876
PORT_MAX = 9895

# ---------- defaults ----------

DEFAULT_TERMINAL = (
    'wt -d "{path}" {cmd}' if shutil.which("wt")
    else 'start "" /D "{path}" cmd /K "{cmd}"'
)
DEFAULT_AI_ALIASES = "claude, codex"

DEFAULT_THEME: dict = {
    "ui_font_family": "Segoe UI",
    "ui_font_size": 10,
    "mono_font_family": "Cascadia Mono",
    "mono_font_size": 10,
    "window_bg": "#1e1f22",
    "panel_bg": "#2b2d30",
    "row_bg": "#2b2d30",
    "row_selected_bg": "#3b3e44",
    "row_hover_bg": "#34373c",
    "row_border": "#3b3e44",
    "row_text_fg": "#e8eaed",
    "row_meta_fg": "#9aa0a6",
    "accent_fg": "#7aa2f7",
    "status_running_fg": "#9ece6a",
    "status_stopped_fg": "#7d8590",
    "output_bg": "#16181d",
    "output_fg": "#d4d4d4",
    "button_bg": "#3b3e44",
    "button_hover_bg": "#4a4e55",
    "button_text_fg": "#e8eaed",
    "input_bg": "#2b2d30",
    "input_text_fg": "#e8eaed",
}

THEME_FRIGUS_NOX = {
    **DEFAULT_THEME,
    "window_bg": "#21252b",
    "panel_bg": "#2c3037",
    "row_bg": "#2c3037",
    "row_selected_bg": "#3a4f6b",
    "row_hover_bg": "#363b44",
    "row_border": "#363b44",
    "row_text_fg": "#e7e9eb",
    "row_meta_fg": "#858585",
    "accent_fg": "#6d95c0",
    "status_running_fg": "#79c3ab",
    "status_stopped_fg": "#587584",
    "output_bg": "#21252b",
    "output_fg": "#c6c6c6",
    "button_bg": "#2c3037",
    "button_hover_bg": "#363b44",
    "button_text_fg": "#e7e9eb",
    "input_bg": "#21252b",
    "input_text_fg": "#e7e9eb",
}

THEME_LIGHT = {
    **DEFAULT_THEME,
    "window_bg": "#fafbfc",
    "panel_bg": "#eef0f3",
    "row_bg": "#ffffff",
    "row_selected_bg": "#e3edff",
    "row_hover_bg": "#f3f5f8",
    "row_border": "#d0d4d9",
    "row_text_fg": "#1f2328",
    "row_meta_fg": "#656d76",
    "accent_fg": "#0969da",
    "status_running_fg": "#1a7f37",
    "status_stopped_fg": "#6e7681",
    "output_bg": "#f6f8fa",
    "output_fg": "#1f2328",
    "button_bg": "#ffffff",
    "button_hover_bg": "#eef0f3",
    "button_text_fg": "#1f2328",
    "input_bg": "#ffffff",
    "input_text_fg": "#1f2328",
}

BUILTIN_THEMES: dict[str, dict] = {
    "Tokyo Night (default)": DEFAULT_THEME,
    "Frigus Nox": THEME_FRIGUS_NOX,
    "Light": THEME_LIGHT,
}

CUSTOM_PRESET = "Custom"


COLOR_KEYS = [
    ("window_bg", "Window background"),
    ("panel_bg", "Panel background"),
    ("row_bg", "Row background"),
    ("row_selected_bg", "Selected row"),
    ("row_hover_bg", "Hover row"),
    ("row_border", "Row border"),
    ("row_text_fg", "Row text"),
    ("row_meta_fg", "Row meta"),
    ("accent_fg", "Accent / selected border"),
    ("status_running_fg", "Status running"),
    ("status_stopped_fg", "Status stopped"),
    ("output_bg", "Output background"),
    ("output_fg", "Output text"),
    ("button_bg", "Button"),
    ("button_hover_bg", "Button hover"),
    ("button_text_fg", "Button text"),
    ("input_bg", "Input background"),
    ("input_text_fg", "Input text"),
]


# ---------- config ----------

def load_config() -> dict:
    if CONFIG.is_file():
        try:
            return json.loads(CONFIG.read_text(encoding="utf-8"))
        except Exception:
            pass
    return {}


def save_config(cfg: dict) -> None:
    CONFIG.write_text(json.dumps(cfg, indent=2), encoding="utf-8")


# ---------- self-update ----------

UPDATER_PS1 = r'''
param(
    [int]$WaitPid,
    [string]$Zip,
    [string]$Install,
    [string]$Exe
)
$ErrorActionPreference = "Stop"
$log = Join-Path $env:TEMP "blinker_update.log"
function Log($msg) {
    $line = "[{0}] {1}" -f (Get-Date -Format "yyyy-MM-dd HH:mm:ss"), $msg
    Add-Content -Path $log -Value $line -Encoding utf8
}
"" | Set-Content -Path $log -Encoding utf8
Log "updater start pid=$PID waitpid=$WaitPid zip=$Zip install=$Install exe=$Exe ps=$($PSVersionTable.PSVersion)"

$leaf = Split-Path -Leaf $Install
$parent = Split-Path -Parent $Install
$bak  = "$Install.bak"

function Wait-ForExit($targetPid, $timeoutMs) {
    $deadline = (Get-Date).AddMilliseconds($timeoutMs)
    while ((Get-Date) -lt $deadline) {
        if (-not (Get-Process -Id $targetPid -ErrorAction SilentlyContinue)) { return $true }
        Start-Sleep -Milliseconds 200
    }
    return $false
}

function Try-Rename($from, $to, $attempts = 30) {
    for ($i = 0; $i -lt $attempts; $i++) {
        try { Rename-Item -Path $from -NewName $to -ErrorAction Stop; return } catch {
            if ($i -eq ($attempts - 1)) { throw }
            Start-Sleep -Milliseconds 500
        }
    }
}

try {
    if (-not (Wait-ForExit $WaitPid 20000)) { Log "WARN: pid $WaitPid still alive after 20s" } else { Log "parent exited" }
    Start-Sleep -Milliseconds 500

    $stage = Join-Path $env:TEMP "blinker_update_stage"
    if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
    Log "expanding $Zip -> $stage"
    Expand-Archive -Path $Zip -DestinationPath $stage -Force

    $sub = Get-ChildItem -Path $stage -Directory | Select-Object -First 1
    if ($sub) { $src = $sub.FullName } else { $src = $stage }
    Log "staged source: $src"

    if (Test-Path $bak) { Remove-Item -Recurse -Force $bak }
    if (Test-Path $Install) {
        Log "renaming install -> bak"
        Try-Rename $Install ($leaf + ".bak")
    }
    Log "moving staged -> install"
    Move-Item -Path $src -Destination $Install

    $newExe = Join-Path $Install (Split-Path -Leaf $Exe)
    Log "launching $newExe"
    if (-not (Test-Path $newExe)) { throw "new exe not found at $newExe" }
    Start-Process -FilePath $newExe -WorkingDirectory $Install

    Remove-Item -Recurse -Force $bak   -ErrorAction SilentlyContinue
    Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
    Remove-Item -Force $Zip            -ErrorAction SilentlyContinue
    Log "updater ok"
} catch {
    Log "ERROR: $_"
    Log $_.ScriptStackTrace
    if ((Test-Path $bak) -and -not (Test-Path $Install)) {
        try { Rename-Item -Path $bak -NewName $leaf; Log "rolled back from bak" } catch { Log "rollback failed: $_" }
    }
    try {
        Add-Type -AssemblyName System.Windows.Forms
        [System.Windows.Forms.MessageBox]::Show("BlinkerUI update failed:`n$_`n`nLog: $log", "BlinkerUI", 0, 16) | Out-Null
    } catch { Log "messagebox failed: $_" }
    # On failure, try to relaunch the existing install so the user isn't left without an app.
    try {
        $oldExe = Join-Path $Install (Split-Path -Leaf $Exe)
        if (Test-Path $oldExe) { Start-Process -FilePath $oldExe -WorkingDirectory $Install }
    } catch { Log "relaunch failed: $_" }
}
'''


def _parse_semver(s: str) -> tuple[int, int, int]:
    s = (s or "").strip().lstrip("vV").split("-")[0].split("+")[0]
    parts = (s.split(".") + ["0", "0", "0"])[:3]
    out = []
    for p in parts:
        try:
            out.append(int(p))
        except ValueError:
            out.append(0)
    return out[0], out[1], out[2]


# WinHTTP-backed HTTP client (avoids _ssl/libcrypto/libssl in the bundle).

class HTTPError(Exception):
    def __init__(self, code: int, reason: str = "") -> None:
        self.code = code
        self.reason = reason
        super().__init__(f"HTTP {code} {reason}".rstrip())


_winhttp = ctypes.WinDLL("winhttp")

_WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY = 4
_WINHTTP_FLAG_SECURE = 0x00800000
_WINHTTP_QUERY_STATUS_CODE = 19
_WINHTTP_QUERY_FLAG_NUMBER = 0x20000000
_WINHTTP_ADDREQ_FLAG_ADD = 0x20000000
_WINHTTP_ADDREQ_FLAG_REPLACE = 0x80000000
_WINHTTP_OPTION_REDIRECT_POLICY = 88
_WINHTTP_OPTION_REDIRECT_POLICY_ALWAYS = 1

_winhttp.WinHttpOpen.argtypes = [
    wintypes.LPCWSTR, wintypes.DWORD, wintypes.LPCWSTR, wintypes.LPCWSTR, wintypes.DWORD
]
_winhttp.WinHttpOpen.restype = wintypes.HANDLE
_winhttp.WinHttpConnect.argtypes = [
    wintypes.HANDLE, wintypes.LPCWSTR, ctypes.c_uint16, wintypes.DWORD
]
_winhttp.WinHttpConnect.restype = wintypes.HANDLE
_winhttp.WinHttpOpenRequest.argtypes = [
    wintypes.HANDLE, wintypes.LPCWSTR, wintypes.LPCWSTR, wintypes.LPCWSTR,
    wintypes.LPCWSTR, ctypes.c_void_p, wintypes.DWORD,
]
_winhttp.WinHttpOpenRequest.restype = wintypes.HANDLE
_winhttp.WinHttpAddRequestHeaders.argtypes = [
    wintypes.HANDLE, wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD
]
_winhttp.WinHttpAddRequestHeaders.restype = wintypes.BOOL
_winhttp.WinHttpSendRequest.argtypes = [
    wintypes.HANDLE, wintypes.LPCWSTR, wintypes.DWORD, ctypes.c_void_p,
    wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p,
]
_winhttp.WinHttpSendRequest.restype = wintypes.BOOL
_winhttp.WinHttpReceiveResponse.argtypes = [wintypes.HANDLE, ctypes.c_void_p]
_winhttp.WinHttpReceiveResponse.restype = wintypes.BOOL
_winhttp.WinHttpQueryHeaders.argtypes = [
    wintypes.HANDLE, wintypes.DWORD, wintypes.LPCWSTR, ctypes.c_void_p,
    ctypes.POINTER(wintypes.DWORD), ctypes.POINTER(wintypes.DWORD),
]
_winhttp.WinHttpQueryHeaders.restype = wintypes.BOOL
_winhttp.WinHttpReadData.argtypes = [
    wintypes.HANDLE, ctypes.c_void_p, wintypes.DWORD, ctypes.POINTER(wintypes.DWORD)
]
_winhttp.WinHttpReadData.restype = wintypes.BOOL
_winhttp.WinHttpCloseHandle.argtypes = [wintypes.HANDLE]
_winhttp.WinHttpCloseHandle.restype = wintypes.BOOL
_winhttp.WinHttpSetOption.argtypes = [
    wintypes.HANDLE, wintypes.DWORD, ctypes.c_void_p, wintypes.DWORD
]
_winhttp.WinHttpSetOption.restype = wintypes.BOOL
_winhttp.WinHttpSetTimeouts.argtypes = [
    wintypes.HANDLE, ctypes.c_int, ctypes.c_int, ctypes.c_int, ctypes.c_int
]
_winhttp.WinHttpSetTimeouts.restype = wintypes.BOOL


def _winhttp_stream(url: str, headers: dict, sink, timeout: float) -> None:
    parts = urlsplit(url)
    if parts.scheme not in ("http", "https"):
        raise ValueError(f"unsupported scheme: {parts.scheme!r}")
    secure = parts.scheme == "https"
    host = parts.hostname or ""
    port = parts.port or (443 if secure else 80)
    path = parts.path or "/"
    if parts.query:
        path += "?" + parts.query

    h_session = _winhttp.WinHttpOpen(
        f"BlinkerUI/{__version__}", _WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, None, None, 0,
    )
    if not h_session:
        raise OSError(f"WinHttpOpen failed (err={ctypes.get_last_error()})")
    try:
        ms = max(1000, int(timeout * 1000))
        _winhttp.WinHttpSetTimeouts(h_session, ms, ms, ms, ms)
        policy = wintypes.DWORD(_WINHTTP_OPTION_REDIRECT_POLICY_ALWAYS)
        _winhttp.WinHttpSetOption(
            h_session, _WINHTTP_OPTION_REDIRECT_POLICY,
            ctypes.byref(policy), ctypes.sizeof(policy),
        )

        h_conn = _winhttp.WinHttpConnect(h_session, host, port, 0)
        if not h_conn:
            raise OSError(f"WinHttpConnect failed (err={ctypes.get_last_error()})")
        try:
            flags = _WINHTTP_FLAG_SECURE if secure else 0
            h_req = _winhttp.WinHttpOpenRequest(
                h_conn, "GET", path, None, None, None, flags,
            )
            if not h_req:
                raise OSError(f"WinHttpOpenRequest failed (err={ctypes.get_last_error()})")
            try:
                hdr_str = "\r\n".join(f"{k}: {v}" for k, v in headers.items())
                if hdr_str and not _winhttp.WinHttpAddRequestHeaders(
                    h_req, hdr_str, len(hdr_str),
                    _WINHTTP_ADDREQ_FLAG_ADD | _WINHTTP_ADDREQ_FLAG_REPLACE,
                ):
                    raise OSError(f"WinHttpAddRequestHeaders failed (err={ctypes.get_last_error()})")
                if not _winhttp.WinHttpSendRequest(h_req, None, 0, None, 0, 0, None):
                    raise OSError(f"WinHttpSendRequest failed (err={ctypes.get_last_error()})")
                if not _winhttp.WinHttpReceiveResponse(h_req, None):
                    raise OSError(f"WinHttpReceiveResponse failed (err={ctypes.get_last_error()})")

                status = wintypes.DWORD(0)
                size = wintypes.DWORD(ctypes.sizeof(status))
                idx = wintypes.DWORD(0)
                if not _winhttp.WinHttpQueryHeaders(
                    h_req, _WINHTTP_QUERY_STATUS_CODE | _WINHTTP_QUERY_FLAG_NUMBER,
                    None, ctypes.byref(status), ctypes.byref(size), ctypes.byref(idx),
                ):
                    raise OSError(f"WinHttpQueryHeaders failed (err={ctypes.get_last_error()})")
                if status.value >= 400:
                    raise HTTPError(status.value)

                buf = ctypes.create_string_buffer(64 * 1024)
                read = wintypes.DWORD(0)
                while True:
                    if not _winhttp.WinHttpReadData(
                        h_req, buf, len(buf), ctypes.byref(read),
                    ):
                        raise OSError(f"WinHttpReadData failed (err={ctypes.get_last_error()})")
                    if read.value == 0:
                        break
                    sink.write(buf.raw[:read.value])
            finally:
                _winhttp.WinHttpCloseHandle(h_req)
        finally:
            _winhttp.WinHttpCloseHandle(h_conn)
    finally:
        _winhttp.WinHttpCloseHandle(h_session)


def _http_json(url: str, timeout: float = 10.0) -> dict:
    sink = io.BytesIO()
    _winhttp_stream(
        url,
        {
            "User-Agent": f"BlinkerUI/{__version__}",
            "Accept": "application/vnd.github+json",
        },
        sink, timeout,
    )
    return json.loads(sink.getvalue().decode("utf-8"))


def _http_download(url: str, dest: Path, timeout: float = 60.0) -> None:
    with open(dest, "wb") as f:
        _winhttp_stream(
            url, {"User-Agent": f"BlinkerUI/{__version__}"}, f, timeout,
        )


def _sha256_hex(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def _find_release_assets(release: dict) -> tuple[dict | None, dict | None]:
    """Return (zip_asset, sha256_asset)."""
    zip_a = sha_a = None
    for a in release.get("assets") or []:
        name = (a.get("name") or "").lower()
        if name.endswith(".zip.sha256"):
            sha_a = a
        elif name.endswith(".zip"):
            zip_a = a
    return zip_a, sha_a


def _spawn_updater(zip_path: Path) -> None:
    install_dir = APP_DIR
    exe_path = Path(sys.executable)
    script_path = Path(tempfile.gettempdir()) / "blinker_updater.ps1"
    script_path.write_text(UPDATER_PS1, encoding="utf-8")
    args = [
        "powershell.exe",
        "-NoProfile",
        "-ExecutionPolicy", "Bypass",
        "-WindowStyle", "Hidden",
        "-File", str(script_path),
        "-WaitPid", str(os.getpid()),
        "-Zip", str(zip_path),
        "-Install", str(install_dir),
        "-Exe", str(exe_path),
    ]
    DETACHED_PROCESS = 0x00000008
    CREATE_NEW_PROCESS_GROUP = 0x00000200
    subprocess.Popen(
        args,
        creationflags=DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP,
        close_fds=True,
    )


# ---------- TCP / blender helpers ----------

def tcp_send(port: int, msg: str, timeout: float = 3.0) -> str:
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=timeout) as s:
            s.sendall((msg + "\n").encode())
            return s.recv(1024).decode().strip()
    except Exception as exc:
        return f"error: {exc}"


def is_running(port: int) -> bool:
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=1.0) as s:
            s.sendall(b"ping\n")
            return s.recv(64).decode().strip().startswith("pong")
    except Exception:
        return False


SCAN_SKIP_DIRS = {
    ".git", ".github", ".vscode", ".idea", "__pycache__", "node_modules",
    "build", "dist", ".venv", "venv", "env", ".tox", ".pytest_cache",
    ".mypy_cache", ".ruff_cache", "htmlcov", "site-packages", "target",
    "bin", "obj", "out", "templates", "templates_toml", "examples",
    "samples", "tests", "test", "docs", "doc",
}

import re as _re

_BL_INFO_RE = _re.compile(r"^\s*bl_info\s*=\s*\{", _re.MULTILINE)
_TEMPLATE_PATTERNS = (
    _re.compile(r'\bid\s*=\s*"ADDON_ID"'),
    _re.compile(r'\bname\s*=\s*"ADDON_NAME"'),
    _re.compile(r'\bid\s*=\s*"my_example_extension"'),
    _re.compile(r'#\s*Example of manifest file', _re.IGNORECASE),
    _re.compile(r'\bmaintainer\s*=\s*"AUTHOR_NAME"'),
)


def addon_kind(path: Path) -> str | None:
    """Return 'extension', 'legacy', or None. Rejects template scaffolds."""
    manifest = path / "blender_manifest.toml"
    if manifest.is_file():
        try:
            text = manifest.read_text(encoding="utf-8", errors="replace")
        except Exception:
            text = ""
        if any(p.search(text) for p in _TEMPLATE_PATTERNS):
            return None
        return "extension"
    init = path / "__init__.py"
    if init.is_file():
        try:
            text = init.read_text(encoding="utf-8", errors="replace")
            if _BL_INFO_RE.search(text):
                return "legacy"
        except Exception:
            pass
    return None


def find_addons(root: Path, max_depth: int = 3) -> list[tuple[Path, str]]:
    """Walk root up to max_depth looking for addon/extension folders."""
    results: list[tuple[Path, str]] = []

    def walk(p: Path, depth: int) -> None:
        if depth > max_depth:
            return
        try:
            kids = sorted(p.iterdir(), key=lambda c: c.name.lower())
        except (PermissionError, OSError):
            return
        for child in kids:
            if not child.is_dir():
                continue
            if child.name.startswith(".") or child.name in SCAN_SKIP_DIRS:
                continue
            kind = addon_kind(child)
            if kind:
                results.append((child, kind))
                continue  # don't recurse into an addon
            walk(child, depth + 1)

    walk(root, 1)
    return results


def find_blender() -> str | None:
    env = os.environ.get("BLENDER_PATH")
    if env and Path(env).is_file():
        return env
    found = shutil.which("blender")
    if found:
        return found
    if sys.platform == "win32":
        for var in ("PROGRAMFILES", "PROGRAMFILES(X86)"):
            root = os.environ.get(var)
            if not root:
                continue
            bf = Path(root) / "Blender Foundation"
            if not bf.is_dir():
                continue
            for d in sorted(bf.iterdir(), reverse=True):
                exe = d / "blender.exe"
                if exe.is_file():
                    return str(exe)
    else:
        for c in (
            "/snap/bin/blender",
            "/usr/bin/blender",
            "/usr/local/bin/blender",
            Path.home() / "blender" / "blender",
            "/var/lib/flatpak/exports/bin/org.blender.Blender",
        ):
            if Path(c).is_file():
                return str(c)
    return None


# ---------- model ----------

class Folder:
    def __init__(self, data: dict) -> None:
        self.path: str = data.get("path", "")
        self.port: int = int(data.get("port", PORT_BASE))
        self.repo: str = data.get("repo", "blinker") or "blinker"
        self.module: str = data.get("module", "")
        self.blend: str = data.get("blend", "")
        self.favourite: bool = bool(data.get("favourite", False))
        self.proc: QProcess | None = None
        self.output: list[str] = []
        self.running: bool = False
        self.stop_requested: bool = False
        self.last_blender: str | None = None
        self.last_env: QProcessEnvironment | None = None

    def to_dict(self) -> dict:
        return {
            "path": self.path,
            "port": self.port,
            "repo": self.repo,
            "module": self.module,
            "blend": self.blend,
            "favourite": self.favourite,
        }


# ---------- theming ----------

def _write_arrow_svgs(color: str) -> tuple[str, str]:
    """Write themed up/down arrow SVGs to a tmp dir and return their url-friendly paths."""
    cache = Path(tempfile.gettempdir()) / "blinker_ui_arrows"
    cache.mkdir(exist_ok=True)
    safe = color.lstrip("#")
    up = cache / f"up_{safe}.svg"
    down = cache / f"down_{safe}.svg"
    if not up.is_file():
        up.write_text(
            f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 12 12">'
            f'<path d="M2 8 L6 3 L10 8 Z" fill="{color}"/></svg>',
            encoding="utf-8",
        )
    if not down.is_file():
        down.write_text(
            f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 12 12">'
            f'<path d="M2 4 L6 9 L10 4 Z" fill="{color}"/></svg>',
            encoding="utf-8",
        )
    return str(up).replace("\\", "/"), str(down).replace("\\", "/")


def stylesheet(t: dict) -> str:
    arrow_up, arrow_down = _write_arrow_svgs(str(t['row_text_fg']))
    return f"""
    QMainWindow, QDialog {{
        background: {t['window_bg']};
        color: {t['row_text_fg']};
    }}
    QWidget {{
        font-family: '{t['ui_font_family']}';
        font-size: {t['ui_font_size']}pt;
        color: {t['row_text_fg']};
    }}
    QScrollArea {{ background: {t['window_bg']}; border: none; }}
    QScrollArea > QWidget > QWidget {{ background: {t['window_bg']}; }}

    QFrame#folderRow {{
        background: {t['row_bg']};
        border: 1px solid {t['row_border']};
        border-radius: 10px;
    }}
    QFrame#folderRow[selected="true"] {{
        background: {t['row_selected_bg']};
        border: 1px solid {t['accent_fg']};
    }}
    QFrame#folderRow:hover {{
        background: {t['row_hover_bg']};
    }}

    QLabel#pathLabel {{ color: {t['row_text_fg']}; font-weight: 600; }}
    QLabel#metaLabel {{ color: {t['row_meta_fg']}; }}
    QLabel#statusDot[running="true"] {{ color: {t['status_running_fg']}; font-weight: bold; font-size: {int(t['ui_font_size'])+4}pt; }}
    QLabel#statusDot[running="false"] {{ color: {t['status_stopped_fg']}; font-weight: bold; font-size: {int(t['ui_font_size'])+4}pt; }}
    QLabel#sectionDivider {{ color: {t['row_border']}; }}

    QPushButton {{
        background: {t['button_bg']};
        color: {t['button_text_fg']};
        border: 1px solid {t['row_border']};
        border-radius: 6px;
        padding: 5px 12px;
        min-height: 22px;
    }}
    QPushButton:hover {{ background: {t['button_hover_bg']}; }}
    QPushButton:pressed {{ background: {t['accent_fg']}; color: {t['window_bg']}; }}
    QPushButton:disabled {{ color: {t['row_meta_fg']}; }}

    QPushButton#removeBtn {{ min-width: 28px; padding: 4px 6px; }}
    QPushButton#favBtn {{ min-width: 28px; padding: 4px 6px; font-size: {int(t['ui_font_size'])+2}pt; }}
    QPushButton#favBtn[fav="true"] {{ color: #f1c40f; }}
    QPushButton#favBtn[fav="false"] {{ color: {t['row_meta_fg']}; }}

    QPlainTextEdit#outputPane {{
        background: {t['output_bg']};
        color: {t['output_fg']};
        font-family: '{t['mono_font_family']}';
        font-size: {t['mono_font_size']}pt;
        border: 1px solid {t['row_border']};
        border-radius: 8px;
        padding: 6px;
    }}

    QToolBar {{
        background: {t['panel_bg']};
        border: none;
        padding: 6px;
        spacing: 6px;
    }}
    QToolBar QToolButton {{
        background: {t['button_bg']};
        color: {t['button_text_fg']};
        border: 1px solid {t['row_border']};
        border-radius: 6px;
        padding: 5px 12px;
    }}
    QToolBar QToolButton:hover {{ background: {t['button_hover_bg']}; }}

    QLineEdit, QSpinBox, QComboBox {{
        background: {t['input_bg']};
        color: {t['input_text_fg']};
        border: 1px solid {t['row_border']};
        border-radius: 5px;
        padding: 4px 6px;
        selection-background-color: {t['accent_fg']};
    }}
    QSpinBox {{ padding-right: 28px; min-height: 24px; }}
    QSpinBox::up-button {{
        subcontrol-origin: border;
        subcontrol-position: top right;
        width: 26px;
        background: {t['button_bg']};
        border-left: 1px solid {t['row_border']};
        border-bottom: 1px solid {t['row_border']};
        border-top-right-radius: 4px;
    }}
    QSpinBox::down-button {{
        subcontrol-origin: border;
        subcontrol-position: bottom right;
        width: 26px;
        background: {t['button_bg']};
        border-left: 1px solid {t['row_border']};
        border-bottom-right-radius: 4px;
    }}
    QSpinBox::up-button:hover, QSpinBox::down-button:hover {{
        background: {t['button_hover_bg']};
    }}
    QSpinBox::up-button:pressed, QSpinBox::down-button:pressed {{
        background: {t['accent_fg']};
    }}
    QSpinBox::up-arrow {{ image: url({arrow_up}); width: 10px; height: 10px; }}
    QSpinBox::down-arrow {{ image: url({arrow_down}); width: 10px; height: 10px; }}
    QComboBox::drop-down {{
        subcontrol-origin: padding;
        subcontrol-position: center right;
        width: 20px;
        border-left: 1px solid {t['row_border']};
        background: {t['button_bg']};
        border-top-right-radius: 4px;
        border-bottom-right-radius: 4px;
    }}
    QComboBox::drop-down:hover {{ background: {t['button_hover_bg']}; }}
    QComboBox::down-arrow {{
        image: url({arrow_down});
        width: 10px; height: 10px;
    }}
    QComboBox QAbstractItemView {{
        background: {t['input_bg']};
        selection-background-color: {t['accent_fg']};
        color: {t['input_text_fg']};
        border: 1px solid {t['row_border']};
    }}

    QTabWidget::pane {{
        border: 1px solid {t['row_border']};
        border-radius: 6px;
        top: -1px;
    }}
    QTabBar::tab {{
        background: {t['panel_bg']};
        color: {t['row_text_fg']};
        padding: 6px 14px;
        border: 1px solid {t['row_border']};
        border-bottom: none;
        border-top-left-radius: 6px;
        border-top-right-radius: 6px;
    }}
    QTabBar::tab:selected {{ background: {t['row_selected_bg']}; }}
    QTabBar::tab:hover {{ background: {t['row_hover_bg']}; }}

    QSplitter::handle {{ background: {t['panel_bg']}; }}
    QScrollBar:vertical, QScrollBar:horizontal {{
        background: {t['panel_bg']};
        border: none;
    }}
    QScrollBar::handle:vertical, QScrollBar::handle:horizontal {{
        background: {t['button_bg']};
        border-radius: 4px;
        min-height: 24px;
        min-width: 24px;
    }}
    QScrollBar::handle:vertical:hover, QScrollBar::handle:horizontal:hover {{
        background: {t['button_hover_bg']};
    }}
    QScrollBar::add-line, QScrollBar::sub-line {{ height: 0px; width: 0px; }}
    QScrollBar::add-page, QScrollBar::sub-page {{ background: transparent; }}
    """


def repolish(w: QWidget) -> None:
    s = w.style()
    if s is not None:
        s.unpolish(w)
        s.polish(w)


# ---------- folder row widget ----------

class FolderRow(QFrame):
    selected = Signal()
    launchClicked = Signal()
    reloadClicked = Signal()
    restartClicked = Signal()
    killClicked = Signal()
    clearClicked = Signal()
    editClicked = Signal()
    removeClicked = Signal()
    favClicked = Signal()
    portChanged = Signal(int)
    aiClicked = Signal(str)

    def __init__(self, folder: Folder, ai_aliases: str, parent: QWidget | None = None) -> None:
        super().__init__(parent)
        self.setObjectName("folderRow")
        self.setProperty("selected", "false")
        self.setCursor(Qt.CursorShape.PointingHandCursor)
        self.folder = folder
        self.ai_aliases = ai_aliases
        self._build()

    def _build(self) -> None:
        h = QHBoxLayout(self)
        h.setContentsMargins(12, 6, 12, 6)
        h.setSpacing(8)

        self.dot = QLabel("○")
        self.dot.setObjectName("statusDot")
        self.dot.setProperty("running", "false")
        h.addWidget(self.dot)

        self.path_lbl = QLabel(self.folder.path)
        self.path_lbl.setObjectName("pathLabel")
        h.addWidget(self.path_lbl)

        self.meta_lbl = QLabel(self._meta_text())
        self.meta_lbl.setObjectName("metaLabel")
        h.addWidget(self.meta_lbl)

        h.addStretch(1)

        port_lbl = QLabel("port")
        port_lbl.setObjectName("metaLabel")
        h.addWidget(port_lbl)

        self.port_e = QSpinBox()
        self.port_e.setRange(1024, 65535)
        self.port_e.setValue(self.folder.port)
        self.port_e.setFixedWidth(130)
        self.port_e.setAlignment(Qt.AlignmentFlag.AlignRight)
        self.port_e.setToolTip("TCP port the bootstrap server listens on")
        self.port_e.editingFinished.connect(self._on_port_edited)
        h.addWidget(self.port_e)

        self.btn_row = QHBoxLayout()
        self.btn_row.setSpacing(6)
        h.addLayout(self.btn_row)

        self.fav_btn = QPushButton()
        self.fav_btn.setObjectName("favBtn")
        self.fav_btn.clicked.connect(self.favClicked)
        h.addWidget(self.fav_btn)
        self._update_fav_btn()

        edit_btn = QPushButton("Edit")
        edit_btn.clicked.connect(self.editClicked)
        h.addWidget(edit_btn)

        rm_btn = QPushButton("✕")
        rm_btn.setObjectName("removeBtn")
        rm_btn.clicked.connect(self.removeClicked)
        h.addWidget(rm_btn)

        self._build_buttons()

    def _meta_text(self) -> str:
        f = self.folder
        parts: list[str] = []
        if f.module:
            parts.append(f"module={f.module}")
        if f.repo and f.repo != "blinker":
            parts.append(f"repo={f.repo}")
        if f.blend:
            parts.append(f"blend={Path(f.blend).name}")
        return "   ·   ".join(parts)

    def _on_port_edited(self) -> None:
        v = int(self.port_e.value())
        if v != self.folder.port:
            self.portChanged.emit(v)

    @staticmethod
    def _clear_layout(layout: QHBoxLayout) -> None:
        while layout.count():
            item = layout.takeAt(0)
            if item is None:
                continue
            w = item.widget()
            if w is not None:
                w.deleteLater()

    def _build_buttons(self) -> None:
        self._clear_layout(self.btn_row)

        running = self.folder.running

        primary = QPushButton("Kill" if running else "Launch")
        primary.clicked.connect(self.killClicked if running else self.launchClicked)
        self.btn_row.addWidget(primary)

        reload_btn = QPushButton("Reload")
        reload_btn.clicked.connect(self.reloadClicked)
        reload_btn.setEnabled(running)
        self.btn_row.addWidget(reload_btn)

        restart_btn = QPushButton("Restart")
        restart_btn.clicked.connect(self.restartClicked)
        restart_btn.setEnabled(running)
        self.btn_row.addWidget(restart_btn)

        clr = QPushButton("Clear console")
        clr.clicked.connect(self.clearClicked)
        self.btn_row.addWidget(clr)

        ai_list = [a.strip() for a in self.ai_aliases.split(",") if a.strip()]
        if ai_list:
            div = QLabel("│")
            div.setObjectName("sectionDivider")
            self.btn_row.addWidget(div)
            for ai in ai_list:
                icon = find_alias_icon(ai)
                if icon is not None:
                    btn = QPushButton()
                    btn.setIcon(icon)
                    btn.setIconSize(QSize(18, 18))
                    btn.setToolTip(ai)
                    btn.setFixedWidth(40)
                else:
                    btn = QPushButton(ai)
                btn.clicked.connect(lambda _checked=False, a=ai: self.aiClicked.emit(a))
                self.btn_row.addWidget(btn)

    def update_state(self) -> None:
        self.path_lbl.setText(self.folder.path)
        self.meta_lbl.setText(self._meta_text())
        if self.port_e.value() != self.folder.port:
            self.port_e.blockSignals(True)
            self.port_e.setValue(self.folder.port)
            self.port_e.blockSignals(False)
        running = self.folder.running
        self.dot.setText("●" if running else "○")
        self.dot.setProperty("running", "true" if running else "false")
        repolish(self.dot)
        self._update_fav_btn()
        self._build_buttons()

    def _update_fav_btn(self) -> None:
        fav = self.folder.favourite
        self.fav_btn.setText("★" if fav else "☆")
        self.fav_btn.setToolTip("Unpin from top" if fav else "Pin to top")
        self.fav_btn.setProperty("fav", "true" if fav else "false")
        repolish(self.fav_btn)

    def set_selected(self, sel: bool) -> None:
        self.setProperty("selected", "true" if sel else "false")
        repolish(self)

    def set_ai_aliases(self, ai_aliases: str) -> None:
        self.ai_aliases = ai_aliases
        self._build_buttons()

    def mousePressEvent(self, ev) -> None:
        self.selected.emit()
        super().mousePressEvent(ev)


# ---------- main window ----------

class MainWindow(QMainWindow):
    statusReady = Signal(dict)
    terminalError = Signal(str, str, str)  # ai, cmd, err_text

    def __init__(self) -> None:
        super().__init__()
        self.setWindowTitle(f"Blinker UI  v{__version__}")
        self.resize(1100, 820)

        cfg = load_config()
        self.folders: list[Folder] = [Folder(d) for d in cfg.get("folders", [])]
        # initial sort: pinned favourites at top
        self.folders.sort(key=lambda f: not f.favourite)
        self.terminal: str = cfg.get("terminal", DEFAULT_TERMINAL)
        self.ai_aliases: str = cfg.get("ai_aliases", DEFAULT_AI_ALIASES)
        self.blender_path: str = cfg.get("blender_path", "")
        self.theme: dict = {**DEFAULT_THEME, **cfg.get("theme", {})}
        self.selected_idx: int | None = 0 if self.folders else None
        self.rows: list[FolderRow] = []
        self.empty_label: QLabel | None = None
        self._poll_in_flight = False

        self._build_ui()
        self._apply_theme()
        self._refresh_rows()

        self.statusReady.connect(self._apply_statuses)
        self.terminalError.connect(self._on_terminal_error)
        self.poll_timer = QTimer(self)
        self.poll_timer.setInterval(2000)
        self.poll_timer.timeout.connect(self._poll_status)
        self.poll_timer.start()
        QTimer.singleShot(0, self._poll_status)

    def _build_ui(self) -> None:
        tb = QToolBar("Main")
        tb.setMovable(False)
        tb.setIconSize(QSize(16, 16))
        self.addToolBar(tb)

        add_act = QAction("+ Add Folder", self)
        add_act.triggered.connect(self._add_folder)
        tb.addAction(add_act)

        settings_act = QAction("Settings…", self)
        settings_act.triggered.connect(self._open_settings)
        tb.addAction(settings_act)

        update_act = QAction("Check for updates", self)
        update_act.triggered.connect(self._check_for_updates)
        tb.addAction(update_act)

        spacer = QWidget()
        spacer.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Preferred)
        tb.addWidget(spacer)

        cfg_lbl = QLabel(f"Config: {CONFIG}")
        cfg_lbl.setObjectName("metaLabel")
        cfg_lbl.setContentsMargins(0, 0, 8, 0)
        tb.addWidget(cfg_lbl)

        splitter = QSplitter(Qt.Orientation.Vertical)
        splitter.setChildrenCollapsible(False)
        self.setCentralWidget(splitter)

        # Folder list
        self.list_scroll = QScrollArea()
        self.list_scroll.setWidgetResizable(True)
        self.list_scroll.setFrameShape(QFrame.Shape.NoFrame)
        self.list_container = QWidget()
        self.list_layout = QVBoxLayout(self.list_container)
        self.list_layout.setContentsMargins(10, 10, 10, 10)
        self.list_layout.setSpacing(8)
        self.list_layout.addStretch(1)
        self.list_scroll.setWidget(self.list_container)
        splitter.addWidget(self.list_scroll)

        # Output pane
        out_container = QWidget()
        ov = QVBoxLayout(out_container)
        ov.setContentsMargins(10, 4, 10, 10)
        ov.setSpacing(6)
        self.out_label = QLabel("(no folder selected)")
        self.out_label.setObjectName("metaLabel")
        ov.addWidget(self.out_label)
        self.output_pane = QPlainTextEdit()
        self.output_pane.setObjectName("outputPane")
        self.output_pane.setReadOnly(True)
        self.output_pane.setMaximumBlockCount(20000)
        ov.addWidget(self.output_pane, 1)
        splitter.addWidget(out_container)

        splitter.setStretchFactor(0, 1)
        splitter.setStretchFactor(1, 2)
        splitter.setSizes([320, 480])

    def _apply_theme(self) -> None:
        app = QApplication.instance()
        if app is not None:
            app.setStyleSheet(stylesheet(self.theme))
            app.setFont(QFont(str(self.theme["ui_font_family"]), int(self.theme["ui_font_size"])))

    def _save(self) -> None:
        save_config({
            "folders": [f.to_dict() for f in self.folders],
            "terminal": self.terminal,
            "ai_aliases": self.ai_aliases,
            "blender_path": self.blender_path,
            "theme": self.theme,
        })

    def _resolve_blender(self) -> str | None:
        if self.blender_path and Path(self.blender_path).is_file():
            return self.blender_path
        return find_blender()

    def _prompt_blender_path(self) -> str | None:
        if sys.platform == "win32":
            filt = "Blender (blender.exe);;All files (*)"
        else:
            filt = "Blender (blender);;All files (*)"
        start = self.blender_path or ""
        path, _ = QFileDialog.getOpenFileName(
            self, "Locate Blender executable", start, filt,
        )
        if path and Path(path).is_file():
            self.blender_path = path
            self._save()
            return path
        return None

    # ----- list rendering -----

    def _refresh_rows(self) -> None:
        for r in self.rows:
            self.list_layout.removeWidget(r)
            r.deleteLater()
        self.rows = []
        if self.empty_label is not None:
            self.list_layout.removeWidget(self.empty_label)
            self.empty_label.deleteLater()
            self.empty_label = None

        if not self.folders:
            empty = QLabel("No folders yet. Click '+ Add Folder' to start.")
            empty.setObjectName("metaLabel")
            empty.setAlignment(Qt.AlignmentFlag.AlignCenter)
            empty.setContentsMargins(0, 24, 0, 24)
            self.empty_label = empty
            self.list_layout.insertWidget(0, empty)
            self._render_output()
            return

        for idx, f in enumerate(self.folders):
            row = FolderRow(f, self.ai_aliases)
            row.update_state()
            row.set_selected(idx == self.selected_idx)
            row.selected.connect(lambda i=idx: self._select(i))
            row.editClicked.connect(lambda i=idx: self._edit_folder(i))
            row.removeClicked.connect(lambda i=idx: self._remove_folder(i))
            row.favClicked.connect(lambda i=idx: self._toggle_fav(i))
            row.launchClicked.connect(lambda i=idx: self._launch(i))
            row.reloadClicked.connect(lambda i=idx: self._reload(i))
            row.restartClicked.connect(lambda i=idx: self._restart(i))
            row.killClicked.connect(lambda i=idx: self._kill(i))
            row.portChanged.connect(lambda p, i=idx: self._change_port(i, p))
            row.clearClicked.connect(lambda i=idx: self._clear(i))
            row.aiClicked.connect(lambda ai, i=idx: self._open_ai(i, ai))
            self.list_layout.insertWidget(idx, row)
            self.rows.append(row)

        self._render_output()

    def _select(self, idx: int) -> None:
        if self.selected_idx == idx:
            return
        self.selected_idx = idx
        for i, r in enumerate(self.rows):
            r.set_selected(i == idx)
        self._render_output()

    def _render_output(self) -> None:
        if self.selected_idx is None or self.selected_idx >= len(self.folders):
            self.out_label.setText("(no folder selected)")
            self.output_pane.setPlainText("")
            return
        f = self.folders[self.selected_idx]
        self.out_label.setText(f"{f.path}   ·   port {f.port}")
        self.output_pane.setPlainText("".join(f.output))
        self.output_pane.moveCursor(QTextCursor.MoveOperation.End)
        self.output_pane.ensureCursorVisible()

    def _append_pane(self, text: str) -> None:
        cur = self.output_pane.textCursor()
        cur.movePosition(QTextCursor.MoveOperation.End)
        cur.insertText(text)
        self.output_pane.setTextCursor(cur)
        self.output_pane.ensureCursorVisible()

    # ----- folder actions -----

    def _next_port(self) -> int:
        used = {f.port for f in self.folders}
        p = PORT_BASE
        while p in used:
            p += 1
        return p

    def _add_folder(self) -> None:
        d = QFileDialog.getExistingDirectory(self, "Select addon folder or parent folder to scan")
        if not d:
            return
        root = Path(d)

        if addon_kind(root) is not None:
            to_add: list[tuple[Path, str]] = [(root, addon_kind(root) or "")]
        else:
            found = find_addons(root)
            if not found:
                QMessageBox.information(
                    self, "No addons",
                    f"No Blender addons or extensions found under:\n{root}",
                )
                return
            existing = {str(Path(f.path).resolve()) for f in self.folders}
            dlg = ScanAddonsDialog(self, root, found, existing)
            if dlg.exec() != QDialog.DialogCode.Accepted:
                return
            to_add = dlg.selected

        if not to_add:
            return

        for path, _kind in to_add:
            self.folders.append(Folder({"path": str(path), "port": self._next_port()}))
        self.selected_idx = len(self.folders) - 1
        self._save()
        self._refresh_rows()

    def _edit_folder(self, idx: int) -> None:
        dlg = EditFolderDialog(self.folders[idx], self)
        if dlg.exec() == QDialog.DialogCode.Accepted:
            self._save()
            self._refresh_rows()

    def _change_port(self, idx: int, port: int) -> None:
        if not (0 <= idx < len(self.folders)):
            return
        f = self.folders[idx]
        if f.port == port:
            return
        f.port = port
        f.running = False  # state on the new port is unknown until next poll
        self._save()
        if 0 <= idx < len(self.rows):
            self.rows[idx].update_state()
        QTimer.singleShot(0, self._poll_status)

    def _toggle_fav(self, idx: int) -> None:
        if not (0 <= idx < len(self.folders)):
            return
        target = self.folders[idx]
        target.favourite = not target.favourite
        selected_target = (
            self.folders[self.selected_idx]
            if self.selected_idx is not None and 0 <= self.selected_idx < len(self.folders)
            else None
        )
        # Stable sort: favourites first, preserve relative order otherwise
        indexed = list(enumerate(self.folders))
        indexed.sort(key=lambda pair: (not pair[1].favourite, pair[0]))
        self.folders = [f for _, f in indexed]
        if selected_target is not None and selected_target in self.folders:
            self.selected_idx = self.folders.index(selected_target)
        self._save()
        self._refresh_rows()

    def _sort_folders(self) -> None:
        """Initial sort on load: favourites first, original order otherwise."""
        indexed = list(enumerate(self.folders))
        indexed.sort(key=lambda pair: (not pair[1].favourite, pair[0]))
        self.folders = [f for _, f in indexed]

    def _remove_folder(self, idx: int) -> None:
        f = self.folders[idx]
        if QMessageBox.question(
            self, "Remove",
            f"Remove this folder from the list?\n\n{f.path}",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        ) != QMessageBox.StandardButton.Yes:
            return
        if f.proc is not None and f.proc.state() != QProcess.ProcessState.NotRunning:
            f.stop_requested = True
            f.proc.terminate()
        self.folders.pop(idx)
        if self.selected_idx is not None and self.selected_idx >= len(self.folders):
            self.selected_idx = len(self.folders) - 1 if self.folders else None
        self._save()
        self._refresh_rows()

    # ----- blender lifecycle -----

    def _launch(self, idx: int) -> None:
        f = self.folders[idx]
        addon_path = Path(f.path)
        if not addon_path.is_dir():
            QMessageBox.critical(self, "Launch", f"Path not found:\n{f.path}")
            return
        if not (addon_path / "blender_manifest.toml").exists() and not (addon_path / "__init__.py").exists():
            QMessageBox.critical(
                self, "Launch",
                f"Not a Blender addon (no __init__.py or blender_manifest.toml):\n{f.path}",
            )
            return
        if not BOOTSTRAP.is_file():
            QMessageBox.critical(self, "Launch", f"bootstrap.py not found at:\n{BOOTSTRAP}")
            return
        blender = self._resolve_blender()
        if not blender:
            res = QMessageBox.question(
                self, "Blender not found",
                "Could not locate Blender automatically.\n\n"
                "Pick the Blender executable now?",
                QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
            )
            if res != QMessageBox.StandardButton.Yes:
                return
            blender = self._prompt_blender_path()
            if not blender:
                return

        legacy = not (addon_path / "blender_manifest.toml").exists()
        module = f.module or addon_path.name

        env = QProcessEnvironment.systemEnvironment()
        env.insert("BLINKER_ADDON_PATH", str(addon_path.resolve()))
        env.insert("BLINKER_MODULE", module)
        env.insert("BLINKER_REPO", f.repo or "blinker")
        env.insert("BLINKER_PORT", str(f.port))
        env.insert("BLINKER_LEGACY", "1" if legacy else "")

        f.stop_requested = False
        f.last_blender = blender
        f.last_env = env
        f.output.append(f"$ {blender} --python {BOOTSTRAP}\n")
        f.output.append(f"  addon:  {addon_path.resolve()}\n")
        f.output.append(
            f"  module: {('(legacy) ' + module) if legacy else f'bl_ext.{f.repo}.{module}'}\n"
        )
        f.output.append(f"  port:   {f.port}\n\n")

        self._spawn_blender(idx, blender, env, f.blend or None)
        f.running = True
        if 0 <= idx < len(self.rows):
            self.rows[idx].update_state()
        if idx == self.selected_idx:
            self._render_output()

    def _spawn_blender(self, idx: int, blender: str, env: QProcessEnvironment, blend_file: str | None) -> None:
        f = self.folders[idx]
        proc = QProcess(self)
        proc.setProcessEnvironment(env)
        proc.setProcessChannelMode(QProcess.ProcessChannelMode.MergedChannels)
        proc.readyReadStandardOutput.connect(lambda i=idx: self._on_proc_output(i))
        proc.finished.connect(
            lambda code, _status, i=idx, bl=blender, e=env, b=blend_file:
            self._on_proc_finished(i, code, bl, e, b)
        )
        args = ["--python", str(BOOTSTRAP)]
        if blend_file:
            args.append(blend_file)
        f.proc = proc
        proc.start(blender, args)

    def _on_proc_output(self, idx: int) -> None:
        if idx >= len(self.folders):
            return
        f = self.folders[idx]
        if f.proc is None:
            return
        data = bytes(f.proc.readAllStandardOutput()).decode("utf-8", errors="replace")
        if not data:
            return
        f.output.append(data)
        if idx == self.selected_idx:
            self._append_pane(data)

    def _on_proc_finished(
        self, idx: int, code: int, blender: str,
        env: QProcessEnvironment, blend_file: str | None,
    ) -> None:
        if idx >= len(self.folders):
            return
        f = self.folders[idx]
        f.output.append(f"\n[blender exited code {code}]\n")
        if idx == self.selected_idx:
            self._append_pane(f"\n[blender exited code {code}]\n")

        if code == 75 and not f.stop_requested:
            restart_marker = os.path.join(tempfile.gettempdir(), "blinker_restart_path")
            blend = blend_file
            if os.path.isfile(restart_marker):
                try:
                    blend = Path(restart_marker).read_text(encoding="utf-8").strip() or blend_file
                    os.remove(restart_marker)
                except Exception:
                    pass
            f.output.append("\n[restarting blender...]\n\n")
            if idx == self.selected_idx:
                self._append_pane("\n[restarting blender...]\n\n")
            self._spawn_blender(idx, blender, env, blend)
        else:
            for p in (
                os.path.join(tempfile.gettempdir(), "blinker_restart_path"),
                os.path.join(tempfile.gettempdir(), "blinker_restart.blend"),
            ):
                try:
                    os.remove(p)
                except FileNotFoundError:
                    pass
                except Exception:
                    pass
            f.proc = None
            f.running = False
            if 0 <= idx < len(self.rows):
                self.rows[idx].update_state()

    def _reload(self, idx: int) -> None:
        f = self.folders[idx]
        resp = tcp_send(f.port, "reload")
        line = f"[reload] {resp}\n"
        f.output.append(line)
        if idx == self.selected_idx:
            self._append_pane(line)

    def _restart(self, idx: int) -> None:
        f = self.folders[idx]
        resp = tcp_send(f.port, "restart")
        line = f"[restart] {resp}\n"
        f.output.append(line)
        if idx == self.selected_idx:
            self._append_pane(line)

    def _kill(self, idx: int) -> None:
        f = self.folders[idx]
        if f.proc is not None and f.proc.state() != QProcess.ProcessState.NotRunning:
            f.stop_requested = True
            f.proc.kill()
            line = "[kill] process killed\n"
        else:
            resp = tcp_send(f.port, "kill")
            line = f"[kill] {resp}\n"
        f.output.append(line)
        if idx == self.selected_idx:
            self._append_pane(line)

    def _clear(self, idx: int) -> None:
        self.folders[idx].output = []
        if idx == self.selected_idx:
            self.output_pane.clear()

    def _open_ai(self, idx: int, ai: str) -> None:
        f = self.folders[idx]
        if not self.terminal.strip():
            QMessageBox.warning(self, "Open AI", "Terminal alias not set (Settings → Terminal).")
            return
        cmd = self.terminal.replace("{path}", f.path).replace("{cmd}", ai)
        threading.Thread(
            target=self._run_terminal, args=(ai, cmd, f.path), daemon=True,
        ).start()

    def _run_terminal(self, ai: str, cmd: str, cwd: str) -> None:
        kw: dict = {}
        if sys.platform == "win32":
            kw["creationflags"] = subprocess.CREATE_NO_WINDOW
        try:
            proc = subprocess.Popen(
                cmd, shell=True, cwd=cwd,
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                encoding="utf-8", errors="replace", **kw,
            )
        except Exception as exc:
            self.terminalError.emit(ai, cmd, str(exc))
            return
        try:
            out, err = proc.communicate(timeout=2.0)
        except subprocess.TimeoutExpired:
            return  # still running — terminal window open, all good
        if proc.returncode != 0:
            msg = (err or out or "").strip() or f"exited with code {proc.returncode}"
            self.terminalError.emit(ai, cmd, msg)

    def _on_terminal_error(self, ai: str, cmd: str, err: str) -> None:
        QMessageBox.critical(
            self, "Terminal failed",
            f"Failed to launch '{ai}'.\n\nCommand:\n  {cmd}\n\nError:\n  {err}",
        )

    def _poll_status(self) -> None:
        if self._poll_in_flight or not self.folders:
            return
        ports = [f.port for f in self.folders]
        self._poll_in_flight = True
        threading.Thread(target=self._probe_thread, args=(ports,), daemon=True).start()

    def _probe_thread(self, ports: list[int]) -> None:
        try:
            workers = min(32, max(1, len(ports)))
            with ThreadPoolExecutor(max_workers=workers) as ex:
                results = list(ex.map(is_running, ports))
            self.statusReady.emit(dict(zip(ports, results)))
        finally:
            self._poll_in_flight = False

    def _apply_statuses(self, statuses: dict) -> None:
        changed_indices: list[int] = []
        for i, f in enumerate(self.folders):
            proc_alive = (
                f.proc is not None
                and f.proc.state() != QProcess.ProcessState.NotRunning
            )
            r = bool(statuses.get(f.port, False)) or proc_alive
            if r != f.running:
                f.running = r
                changed_indices.append(i)
        if not changed_indices:
            return
        for i in changed_indices:
            if 0 <= i < len(self.rows):
                self.rows[i].update_state()

    def _open_settings(self) -> None:
        dlg = SettingsDialog(
            self, self.theme, self.terminal, self.ai_aliases, self.blender_path,
        )
        if dlg.exec() == QDialog.DialogCode.Accepted:
            self.theme = dict(dlg.draft_theme)
            self.terminal = dlg.terminal_value
            self.ai_aliases = dlg.ai_aliases_value
            self.blender_path = dlg.blender_path_value
            self._save()
            self._apply_theme()
            self._refresh_rows()

    def _check_for_updates(self) -> None:
        if not FROZEN:
            QMessageBox.information(
                self, "Updates",
                f"Running from source (v{__version__}).\n"
                "Self-update only applies to packaged builds.",
            )
            return
        try:
            release = _http_json(UPDATE_API)
        except HTTPError as e:
            QMessageBox.warning(
                self, "Updates",
                f"GitHub returned HTTP {e.code}. Check the repo URL or try later.",
            )
            return
        except Exception as e:
            QMessageBox.warning(self, "Updates", f"Could not check for updates:\n{e}")
            return

        tag = (release.get("tag_name") or "").strip()
        cur = _parse_semver(__version__)
        new = _parse_semver(tag)
        if not tag or new <= cur:
            QMessageBox.information(
                self, "Updates",
                f"You are on the latest version (v{__version__}).",
            )
            return

        zip_asset, sha_asset = _find_release_assets(release)
        if zip_asset is None:
            QMessageBox.warning(
                self, "Updates",
                f"Release {tag} has no .zip asset to install.",
            )
            return

        notes = (release.get("body") or "").strip()
        notes_preview = ("\n\n" + notes[:500] + ("…" if len(notes) > 500 else "")) if notes else ""
        res = QMessageBox.question(
            self, "Update available",
            f"A new version is available.\n\n"
            f"Current:  v{__version__}\nLatest:   {tag}\n\n"
            f"BlinkerUI will download, close, replace itself, and relaunch."
            f"{notes_preview}",
            QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
        )
        if res != QMessageBox.StandardButton.Yes:
            return

        tmp = Path(tempfile.gettempdir())
        zip_path = tmp / zip_asset["name"]
        try:
            _http_download(zip_asset["browser_download_url"], zip_path)
        except Exception as e:
            QMessageBox.critical(self, "Updates", f"Download failed:\n{e}")
            return

        if sha_asset is not None:
            try:
                sha_path = tmp / sha_asset["name"]
                _http_download(sha_asset["browser_download_url"], sha_path)
                expected = sha_path.read_text(encoding="utf-8").strip().split()[0].lower()
                actual = _sha256_hex(zip_path)
                if expected != actual:
                    zip_path.unlink(missing_ok=True)
                    QMessageBox.critical(
                        self, "Updates",
                        f"SHA256 mismatch — aborting.\n\nexpected {expected}\nactual   {actual}",
                    )
                    return
            except Exception as e:
                cont = QMessageBox.warning(
                    self, "Updates",
                    f"Could not verify SHA256 ({e}).\nProceed anyway?",
                    QMessageBox.StandardButton.Yes | QMessageBox.StandardButton.No,
                )
                if cont != QMessageBox.StandardButton.Yes:
                    zip_path.unlink(missing_ok=True)
                    return

        try:
            _spawn_updater(zip_path)
        except Exception as e:
            QMessageBox.critical(self, "Updates", f"Could not launch updater:\n{e}")
            return

        QApplication.quit()

    def closeEvent(self, ev) -> None:
        for f in self.folders:
            if f.proc is not None and f.proc.state() != QProcess.ProcessState.NotRunning:
                f.stop_requested = True
                f.proc.terminate()
                f.proc.waitForFinished(500)
        super().closeEvent(ev)


# ---------- scan addons dialog ----------

class ScanAddonsDialog(QDialog):
    def __init__(
        self, parent: QWidget, root: Path,
        found: list[tuple[Path, str]], existing_paths: set[str],
    ) -> None:
        super().__init__(parent)
        self.setWindowTitle("Add addons")
        self.resize(780, 520)
        self.selected: list[tuple[Path, str]] = []

        v = QVBoxLayout(self)

        header = QLabel(f"Found {len(found)} addon(s) under:\n{root}")
        header.setObjectName("metaLabel")
        v.addWidget(header)

        self.lst = QListWidget()
        self.lst.setUniformItemSizes(True)
        self.lst.itemDoubleClicked.connect(self._toggle_item)
        for path, kind in found:
            item = QListWidgetItem(f"[{kind:<9}]  {path}")
            item.setData(Qt.ItemDataRole.UserRole, (path, kind))
            already = str(path.resolve()) in existing_paths
            item.setFlags(item.flags() | Qt.ItemFlag.ItemIsUserCheckable)
            if already:
                item.setText(f"[{kind:<9}]  {path}    (already added)")
                item.setFlags(item.flags() & ~Qt.ItemFlag.ItemIsEnabled)
                item.setCheckState(Qt.CheckState.Unchecked)
            else:
                item.setCheckState(Qt.CheckState.Checked)
            self.lst.addItem(item)
        v.addWidget(self.lst, 1)

        sel_row = QHBoxLayout()
        sa = QPushButton("Select all")
        sa.clicked.connect(lambda: self._set_all(Qt.CheckState.Checked))
        sn = QPushButton("Select none")
        sn.clicked.connect(lambda: self._set_all(Qt.CheckState.Unchecked))
        sel_row.addWidget(sa)
        sel_row.addWidget(sn)
        sel_row.addStretch(1)
        v.addLayout(sel_row)

        bb = QDialogButtonBox(
            QDialogButtonBox.StandardButton.Ok | QDialogButtonBox.StandardButton.Cancel
        )
        bb.accepted.connect(self._ok)
        bb.rejected.connect(self.reject)
        v.addWidget(bb)

    def _set_all(self, state: Qt.CheckState) -> None:
        for i in range(self.lst.count()):
            item = self.lst.item(i)
            if item is None:
                continue
            if not (item.flags() & Qt.ItemFlag.ItemIsEnabled):
                continue
            item.setCheckState(state)

    def _toggle_item(self, item: QListWidgetItem) -> None:
        if not (item.flags() & Qt.ItemFlag.ItemIsEnabled):
            return
        item.setCheckState(
            Qt.CheckState.Unchecked if item.checkState() == Qt.CheckState.Checked
            else Qt.CheckState.Checked
        )

    def _ok(self) -> None:
        for i in range(self.lst.count()):
            item = self.lst.item(i)
            if item is None:
                continue
            if item.checkState() == Qt.CheckState.Checked:
                data = item.data(Qt.ItemDataRole.UserRole)
                self.selected.append(data)
        self.accept()


# ---------- edit folder dialog ----------

class EditFolderDialog(QDialog):
    def __init__(self, folder: Folder, parent: QWidget | None = None) -> None:
        super().__init__(parent)
        self.folder = folder
        self.setWindowTitle("Edit folder")
        self.resize(680, 280)

        v = QVBoxLayout(self)
        form = QFormLayout()

        self.path_e = QLineEdit(folder.path)
        path_row = QHBoxLayout()
        path_row.addWidget(self.path_e, 1)
        path_btn = QPushButton("…")
        path_btn.setFixedWidth(36)
        path_btn.clicked.connect(self._browse_path)
        path_row.addWidget(path_btn)
        form.addRow("Path", path_row)

        self.port_e = QSpinBox()
        self.port_e.setRange(1024, 65535)
        self.port_e.setValue(folder.port)
        form.addRow("Port", self.port_e)

        self.repo_e = QLineEdit(folder.repo)
        form.addRow("Repo", self.repo_e)

        self.module_e = QLineEdit(folder.module)
        form.addRow("Module", self.module_e)

        self.blend_e = QLineEdit(folder.blend)
        blend_row = QHBoxLayout()
        blend_row.addWidget(self.blend_e, 1)
        blend_btn = QPushButton("…")
        blend_btn.setFixedWidth(36)
        blend_btn.clicked.connect(self._browse_blend)
        blend_row.addWidget(blend_btn)
        form.addRow("Blend", blend_row)

        v.addLayout(form)

        hint = QLabel("Module/Repo blank = auto. Repo defaults to 'blinker' (extensions only).")
        hint.setObjectName("metaLabel")
        v.addWidget(hint)
        v.addStretch(1)

        bb = QDialogButtonBox(
            QDialogButtonBox.StandardButton.Ok | QDialogButtonBox.StandardButton.Cancel
        )
        bb.accepted.connect(self.accept)
        bb.rejected.connect(self.reject)
        v.addWidget(bb)

    def _browse_path(self) -> None:
        d = QFileDialog.getExistingDirectory(self, "Select addon folder", self.path_e.text())
        if d:
            self.path_e.setText(d)

    def _browse_blend(self) -> None:
        start = self.blend_e.text() or self.path_e.text() or ""
        f, _ = QFileDialog.getOpenFileName(
            self, "Select .blend", start, "Blend files (*.blend);;All files (*)",
        )
        if f:
            self.blend_e.setText(f)

    def accept(self) -> None:
        self.folder.path = self.path_e.text().strip()
        self.folder.port = int(self.port_e.value())
        self.folder.repo = self.repo_e.text().strip() or "blinker"
        self.folder.module = self.module_e.text().strip()
        self.folder.blend = self.blend_e.text().strip()
        super().accept()


# ---------- settings dialog ----------

class SettingsDialog(QDialog):
    def __init__(
        self, parent: QWidget, theme: dict, terminal: str,
        ai_aliases: str, blender_path: str,
    ) -> None:
        super().__init__(parent)
        self.setWindowTitle("Settings")
        self.resize(760, 720)

        self.draft_theme: dict = dict(theme)
        self.terminal_value: str = terminal
        self.ai_aliases_value: str = ai_aliases
        self.blender_path_value: str = blender_path
        self.swatches: dict[str, QLabel] = {}
        self.hex_edits: dict[str, QLineEdit] = {}

        v = QVBoxLayout(self)

        tabs = QTabWidget()
        tabs.addTab(self._terminal_tab(terminal, ai_aliases, blender_path), "Terminal")
        tabs.addTab(self._fonts_tab(), "Fonts")
        tabs.addTab(self._colors_tab(), "Colors")
        v.addWidget(tabs, 1)

        bb = QDialogButtonBox(
            QDialogButtonBox.StandardButton.Ok
            | QDialogButtonBox.StandardButton.Cancel
            | QDialogButtonBox.StandardButton.Apply
            | QDialogButtonBox.StandardButton.RestoreDefaults
        )
        ok_btn = bb.button(QDialogButtonBox.StandardButton.Ok)
        cancel_btn = bb.button(QDialogButtonBox.StandardButton.Cancel)
        apply_btn = bb.button(QDialogButtonBox.StandardButton.Apply)
        reset_btn = bb.button(QDialogButtonBox.StandardButton.RestoreDefaults)
        if ok_btn is not None:
            ok_btn.clicked.connect(self._ok)
        if cancel_btn is not None:
            cancel_btn.clicked.connect(self.reject)
        if apply_btn is not None:
            apply_btn.clicked.connect(self._apply)
        if reset_btn is not None:
            reset_btn.clicked.connect(self._reset_all)
        v.addWidget(bb)

    # ----- tabs -----

    def _terminal_tab(self, terminal: str, ai_aliases: str, blender_path: str) -> QWidget:
        w = QWidget()
        form = QFormLayout(w)

        self.blender_e = QLineEdit(blender_path)
        self.blender_e.setPlaceholderText("(auto-detect: BLENDER_PATH, PATH, then standard install dirs)")
        blender_row = QHBoxLayout()
        blender_row.addWidget(self.blender_e, 1)
        browse_btn = QPushButton("Browse…")
        browse_btn.clicked.connect(self._browse_blender)
        blender_row.addWidget(browse_btn)
        auto_btn = QPushButton("Auto-detect")
        auto_btn.clicked.connect(self._auto_blender)
        blender_row.addWidget(auto_btn)
        form.addRow("Blender executable", blender_row)

        self.terminal_e = QLineEdit(terminal)
        form.addRow("Terminal alias", self.terminal_e)

        self.ai_e = QLineEdit(ai_aliases)
        form.addRow("AI aliases", self.ai_e)

        hint = QLabel(
            "Blender executable: explicit path overrides auto-detection. Leave blank to auto-detect.\n\n"
            "Terminal alias: shell command run when an AI button is clicked.\n"
            "  {path} = addon folder.   {cmd} = AI alias.\n"
            f"  Default:  {DEFAULT_TERMINAL}\n\n"
            "AI aliases: comma-separated. One button per alias on each row.\n"
            "  Examples:  claude, codex, gemini\n\n"
            "Both run via shell with cwd = addon folder."
        )
        hint.setObjectName("metaLabel")
        hint.setWordWrap(True)
        form.addRow(hint)

        reset = QPushButton("Reset terminal alias")
        reset.clicked.connect(lambda: self.terminal_e.setText(DEFAULT_TERMINAL))
        form.addRow(reset)
        return w

    def _browse_blender(self) -> None:
        if sys.platform == "win32":
            filt = "Blender (blender.exe);;All files (*)"
        else:
            filt = "Blender (blender);;All files (*)"
        start = self.blender_e.text() or ""
        path, _ = QFileDialog.getOpenFileName(
            self, "Locate Blender executable", start, filt,
        )
        if path:
            self.blender_e.setText(path)

    def _auto_blender(self) -> None:
        found = find_blender()
        if found:
            self.blender_e.setText(found)
        else:
            QMessageBox.information(
                self, "Auto-detect",
                "Could not locate Blender automatically.\n"
                "Set BLENDER_PATH or browse to the executable.",
            )

    def _fonts_tab(self) -> QWidget:
        w = QWidget()
        form = QFormLayout(w)
        families = QFontDatabase.families()

        self.ui_family_cb = QComboBox()
        self.ui_family_cb.addItems(families)
        self.ui_family_cb.setEditable(True)
        self.ui_family_cb.setCurrentText(str(self.draft_theme["ui_font_family"]))
        form.addRow("UI font family", self.ui_family_cb)

        self.ui_size_sp = QSpinBox()
        self.ui_size_sp.setRange(7, 24)
        self.ui_size_sp.setValue(int(self.draft_theme["ui_font_size"]))
        form.addRow("UI font size", self.ui_size_sp)

        self.mono_family_cb = QComboBox()
        self.mono_family_cb.addItems(families)
        self.mono_family_cb.setEditable(True)
        self.mono_family_cb.setCurrentText(str(self.draft_theme["mono_font_family"]))
        form.addRow("Mono font family", self.mono_family_cb)

        self.mono_size_sp = QSpinBox()
        self.mono_size_sp.setRange(7, 24)
        self.mono_size_sp.setValue(int(self.draft_theme["mono_font_size"]))
        form.addRow("Mono font size", self.mono_size_sp)

        hint = QLabel("Mono font is used for the output console.")
        hint.setObjectName("metaLabel")
        form.addRow(hint)
        return w

    def _colors_tab(self) -> QWidget:
        w = QWidget()
        v = QVBoxLayout(w)

        preset_row = QHBoxLayout()
        preset_row.addWidget(QLabel("Preset:"))
        self.preset_cb = QComboBox()
        self.preset_cb.addItems(list(BUILTIN_THEMES.keys()) + [CUSTOM_PRESET])
        self.preset_cb.setCurrentText(self._detect_preset())
        self.preset_cb.currentTextChanged.connect(self._on_preset_change)
        preset_row.addWidget(self.preset_cb, 1)

        exp = QPushButton("Export theme…")
        exp.clicked.connect(self._export_theme)
        imp = QPushButton("Import theme…")
        imp.clicked.connect(self._import_theme)
        preset_row.addWidget(exp)
        preset_row.addWidget(imp)
        v.addLayout(preset_row)

        scroll = QScrollArea()
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QFrame.Shape.NoFrame)
        inner = QWidget()
        grid = QGridLayout(inner)
        grid.setContentsMargins(0, 0, 0, 0)
        grid.setHorizontalSpacing(10)
        grid.setVerticalSpacing(8)

        for i, (key, label) in enumerate(COLOR_KEYS):
            grid.addWidget(QLabel(label), i, 0)

            sw = QLabel()
            sw.setFixedSize(32, 22)
            self._set_swatch(sw, str(self.draft_theme[key]))
            self.swatches[key] = sw
            grid.addWidget(sw, i, 1)

            hex_e = QLineEdit(str(self.draft_theme[key]))
            hex_e.setMaxLength(9)
            hex_e.setFixedWidth(110)
            hex_e.editingFinished.connect(lambda k=key: self._commit_hex(k))
            self.hex_edits[key] = hex_e
            grid.addWidget(hex_e, i, 2)

            pick = QPushButton("Pick…")
            pick.clicked.connect(lambda _checked=False, k=key: self._pick(k))
            grid.addWidget(pick, i, 3)

            reset = QPushButton("Reset")
            reset.clicked.connect(lambda _checked=False, k=key: self._reset_one(k))
            grid.addWidget(reset, i, 4)

        grid.setColumnStretch(0, 1)
        scroll.setWidget(inner)
        v.addWidget(scroll, 1)
        return w

    # ----- color helpers -----

    @staticmethod
    def _set_swatch(sw: QLabel, color: str) -> None:
        sw.setStyleSheet(
            f"background-color: {color};"
            " border: 1px solid rgba(127,127,127,0.5);"
            " border-radius: 4px;"
        )

    def _refresh_swatch(self, key: str) -> None:
        color = str(self.draft_theme[key])
        self._set_swatch(self.swatches[key], color)
        self.hex_edits[key].setText(color)

    def _detect_preset(self) -> str:
        for name, theme in BUILTIN_THEMES.items():
            if all(self.draft_theme.get(k) == theme.get(k) for k, _ in COLOR_KEYS):
                return name
        return CUSTOM_PRESET

    def _set_preset_silent(self, name: str) -> None:
        cb = getattr(self, "preset_cb", None)
        if cb is None:
            return
        cb.blockSignals(True)
        idx = cb.findText(name)
        if idx >= 0:
            cb.setCurrentIndex(idx)
        cb.blockSignals(False)

    def _on_preset_change(self, name: str) -> None:
        if name == CUSTOM_PRESET or name not in BUILTIN_THEMES:
            return
        preset = BUILTIN_THEMES[name]
        for k, _ in COLOR_KEYS:
            if k in preset:
                self.draft_theme[k] = preset[k]
            self._refresh_swatch(k)

    @staticmethod
    def _normalize_hex(s: str) -> str | None:
        s = s.strip().lstrip("#")
        if len(s) == 3 and all(c in "0123456789abcdefABCDEF" for c in s):
            s = "".join(c * 2 for c in s)
        if len(s) == 6 and all(c in "0123456789abcdefABCDEF" for c in s):
            return "#" + s.lower()
        return None

    def _commit_hex(self, key: str) -> None:
        norm = self._normalize_hex(self.hex_edits[key].text())
        if norm is None:
            self.hex_edits[key].setText(str(self.draft_theme[key]))
            return
        self.draft_theme[key] = norm
        self._refresh_swatch(key)
        self._set_preset_silent(self._detect_preset())

    def _pick(self, key: str) -> None:
        c = QColorDialog.getColor(QColor(str(self.draft_theme[key])), self, f"Pick {key}")
        if c.isValid():
            self.draft_theme[key] = c.name()
            self._refresh_swatch(key)
            self._set_preset_silent(self._detect_preset())

    def _reset_one(self, key: str) -> None:
        self.draft_theme[key] = DEFAULT_THEME[key]
        self._refresh_swatch(key)
        self._set_preset_silent(self._detect_preset())

    def _reset_all(self) -> None:
        self.draft_theme = dict(DEFAULT_THEME)
        self.ui_family_cb.setCurrentText(str(self.draft_theme["ui_font_family"]))
        self.ui_size_sp.setValue(int(self.draft_theme["ui_font_size"]))
        self.mono_family_cb.setCurrentText(str(self.draft_theme["mono_font_family"]))
        self.mono_size_sp.setValue(int(self.draft_theme["mono_font_size"]))
        for key, _ in COLOR_KEYS:
            self._refresh_swatch(key)
        self.terminal_e.setText(DEFAULT_TERMINAL)
        self.ai_e.setText(DEFAULT_AI_ALIASES)
        self.blender_e.setText("")
        self._set_preset_silent(self._detect_preset())

    # ----- export / import -----

    def _export_theme(self) -> None:
        self._commit_inputs()
        path, _ = QFileDialog.getSaveFileName(
            self, "Export theme", "blinker_theme.json", "JSON theme (*.json)",
        )
        if not path:
            return
        try:
            Path(path).write_text(json.dumps(self.draft_theme, indent=2), encoding="utf-8")
        except Exception as exc:
            QMessageBox.critical(self, "Export failed", str(exc))

    def _import_theme(self) -> None:
        path, _ = QFileDialog.getOpenFileName(
            self, "Import theme", "", "JSON theme (*.json);;All files (*)",
        )
        if not path:
            return
        try:
            data = json.loads(Path(path).read_text(encoding="utf-8"))
            if not isinstance(data, dict):
                raise ValueError("Theme file must be a JSON object")
        except Exception as exc:
            QMessageBox.critical(self, "Import failed", str(exc))
            return

        unknown, invalid = [], []
        for k, v in data.items():
            if k not in DEFAULT_THEME:
                unknown.append(k)
                continue
            if k in {"ui_font_size", "mono_font_size"}:
                try:
                    self.draft_theme[k] = int(v)
                except (TypeError, ValueError):
                    invalid.append(k)
            elif k in {"ui_font_family", "mono_font_family"}:
                self.draft_theme[k] = str(v)
            else:
                norm = self._normalize_hex(str(v))
                if norm is None:
                    invalid.append(k)
                else:
                    self.draft_theme[k] = norm

        self.ui_family_cb.setCurrentText(str(self.draft_theme["ui_font_family"]))
        self.ui_size_sp.setValue(int(self.draft_theme["ui_font_size"]))
        self.mono_family_cb.setCurrentText(str(self.draft_theme["mono_font_family"]))
        self.mono_size_sp.setValue(int(self.draft_theme["mono_font_size"]))
        for key, _ in COLOR_KEYS:
            self._refresh_swatch(key)
        self._set_preset_silent(self._detect_preset())

        msgs = []
        if unknown:
            msgs.append("Ignored unknown keys: " + ", ".join(unknown))
        if invalid:
            msgs.append("Invalid values for: " + ", ".join(invalid))
        if msgs:
            QMessageBox.warning(self, "Import notes", "\n".join(msgs))

    # ----- commit / apply -----

    def _commit_inputs(self) -> None:
        self.draft_theme["ui_font_family"] = (
            self.ui_family_cb.currentText() or DEFAULT_THEME["ui_font_family"]
        )
        self.draft_theme["ui_font_size"] = int(self.ui_size_sp.value())
        self.draft_theme["mono_font_family"] = (
            self.mono_family_cb.currentText() or DEFAULT_THEME["mono_font_family"]
        )
        self.draft_theme["mono_font_size"] = int(self.mono_size_sp.value())
        self.terminal_value = self.terminal_e.text().strip() or DEFAULT_TERMINAL
        self.ai_aliases_value = self.ai_e.text().strip()
        self.blender_path_value = self.blender_e.text().strip()

    def _apply(self) -> None:
        self._commit_inputs()
        parent = self.parent()
        if isinstance(parent, MainWindow):
            parent.theme = dict(self.draft_theme)
            parent.terminal = self.terminal_value
            parent.ai_aliases = self.ai_aliases_value
            parent.blender_path = self.blender_path_value
            parent._save()
            parent._apply_theme()
            parent._refresh_rows()

    def _ok(self) -> None:
        self._commit_inputs()
        self.accept()


# ---------- entry ----------

def _set_windows_aumid(aumid: str) -> None:
    if sys.platform != "win32":
        return
    try:
        ctypes.windll.shell32.SetCurrentProcessExplicitAppUserModelID(aumid)
    except Exception:
        pass


# Win32 single-instance: named mutex for "another instance running?" check,
# and a wake-file polled by a QTimer so the second launch can raise the first.
SINGLETON_MUTEX = "Local\\blinker-ui-singleton"
WAKE_FILE = Path(tempfile.gettempdir()) / "blinker-ui.wake"
_ERROR_ALREADY_EXISTS = 183


def _acquire_singleton() -> int | None:
    """Return mutex handle if we are the first instance, else None."""
    kernel32 = ctypes.windll.kernel32
    kernel32.CreateMutexW.restype = wintypes.HANDLE
    kernel32.CreateMutexW.argtypes = [ctypes.c_void_p, wintypes.BOOL, wintypes.LPCWSTR]
    kernel32.GetLastError.restype = wintypes.DWORD
    h = kernel32.CreateMutexW(None, False, SINGLETON_MUTEX)
    if not h:
        return None
    if kernel32.GetLastError() == _ERROR_ALREADY_EXISTS:
        kernel32.CloseHandle(h)
        return None
    return h


def _signal_wake() -> None:
    try:
        WAKE_FILE.write_text(str(os.getpid()), encoding="ascii")
    except OSError:
        pass


def _install_wake_watcher(window: "MainWindow") -> QTimer:
    try:
        WAKE_FILE.unlink()
    except FileNotFoundError:
        pass
    state = {"mtime": 0.0}

    def tick() -> None:
        try:
            mtime = WAKE_FILE.stat().st_mtime
        except FileNotFoundError:
            return
        if mtime == state["mtime"]:
            return
        state["mtime"] = mtime
        if window.isMinimized():
            window.showNormal()
        else:
            window.show()
        window.raise_()
        window.activateWindow()

    timer = QTimer(window)
    timer.timeout.connect(tick)
    timer.start(750)
    return timer


def main() -> None:
    _set_windows_aumid("blinker.ui")

    mutex = _acquire_singleton()
    if mutex is None:
        _signal_wake()
        return

    app = QApplication(sys.argv)
    app.setApplicationName("Blinker UI")
    app.setOrganizationName("blinker")
    app.setStyle("Fusion")

    if LOGO.is_file():
        icon = QIcon(str(LOGO))
        app.setWindowIcon(icon)
    win = MainWindow()
    win._singleton_mutex = mutex  # type: ignore[attr-defined]
    win._wake_watcher = _install_wake_watcher(win)  # type: ignore[attr-defined]
    win.show()
    sys.exit(app.exec())


if __name__ == "__main__":
    main()
