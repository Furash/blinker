"""Blinker bootstrap - runs inside Blender via --python.

Sets up addon symlink, enables addon, starts TCP reload server.
"""

import os
import socket
import sys
import traceback
from pathlib import Path

import bpy

_addon_path = Path(os.environ["BLINKER_ADDON_PATH"])
_module = os.environ["BLINKER_MODULE"]
_repo = os.environ["BLINKER_REPO"]
_port = int(os.environ["BLINKER_PORT"])
_full_module = f"bl_ext.{_repo}.{_module}"

_server_socket = None


def _log(msg):
    print(f"[blinker] {msg}")


# -- Symlink / junction setup --


def _ensure_repo():
    for repo in bpy.context.preferences.extensions.repos:
        if repo.module == _repo:
            return repo
    _log(f"Creating extensions repo '{_repo}'")
    return bpy.context.preferences.extensions.repos.new(name=_repo, module=_repo)


def _resolve_link(path):
    """Return symlink/junction target as Path, or None if not a link."""
    try:
        target = os.readlink(str(path))
    except OSError as e:
        # WinError 4390 = "not a reparse point" (regular directory, not a link)
        if sys.platform == "win32" and getattr(e, "winerror", None) == 4390:
            return None
        _log(f"  readlink failed for {path}: {e}")
        return None
    except ValueError:
        return None

    # Windows junctions return \\?\ prefixed paths
    if sys.platform == "win32":
        target = target.lstrip("\\\\?")
        target = target.lstrip("?")

    return Path(target).resolve()


def _find_existing_link():
    """Search all extension repos for an existing link to _addon_path."""
    resolved = _addon_path.resolve()
    _log(f"Looking for existing link to {resolved}")
    for repo in bpy.context.preferences.extensions.repos:
        if not repo.enabled:
            continue
        repo_dir = Path(repo.custom_directory if repo.use_custom_directory else repo.directory)
        if not repo_dir.is_dir():
            continue
        for entry in repo_dir.iterdir():
            if not entry.is_dir():
                continue
            target = _resolve_link(entry)
            if target is not None:
                _log(f"  {entry.name} in '{repo.module}' -> {target}")
                if target == resolved:
                    return repo.module, entry
    return None, None


def _create_link():
    global _full_module

    # Check all repos for existing link to same addon (e.g. from VS Code extension)
    existing_repo, existing_path = _find_existing_link()
    if existing_path is not None:
        _full_module = f"bl_ext.{existing_repo}.{existing_path.name}"
        _log(f"Found existing link in '{existing_repo}': {existing_path}")
        return

    # No existing link — create one in blinker's repo
    repo = _ensure_repo()
    repo_dir = Path(repo.custom_directory if repo.use_custom_directory else repo.directory)
    os.makedirs(repo_dir, exist_ok=True)

    link_path = repo_dir / _module

    if link_path.exists() or link_path.is_symlink():
        _log(f"Removing old link: {link_path}")
        os.remove(link_path)

    if sys.platform == "win32":
        import _winapi
        _winapi.CreateJunction(str(_addon_path), str(link_path))
    else:
        os.symlink(str(_addon_path), str(link_path), target_is_directory=True)

    _log(f"Linked: {link_path} -> {_addon_path}")


# -- Addon enable / reload --


def _enable_addon():
    bpy.ops.extensions.repo_refresh_all()
    bpy.ops.preferences.addon_enable(module=_full_module)
    _log(f"Enabled: {_full_module}")


def _reload_addon():
    """Disable -> purge sys.modules -> re-enable. Same technique as VS Code extension."""
    try:
        bpy.ops.preferences.addon_disable(module=_full_module)
    except Exception:
        traceback.print_exc()
        return "error: disable failed"

    count = 0
    for name in list(sys.modules.keys()):
        if name == _full_module or name.startswith(_full_module + "."):
            del sys.modules[name]
            count += 1

    try:
        bpy.ops.preferences.addon_enable(module=_full_module)
    except Exception:
        traceback.print_exc()
        return "error: enable failed"

    for window in bpy.context.window_manager.windows:
        for area in window.screen.areas:
            area.tag_redraw()

    _log(f"Reloaded {_full_module} ({count} modules)")
    return f"ok ({count} modules)"


# -- TCP server --


def _start_server():
    global _server_socket
    _server_socket = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    _server_socket.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
    _server_socket.setblocking(False)
    _server_socket.bind(("127.0.0.1", _port))
    _server_socket.listen(1)
    bpy.app.timers.register(_poll, persistent=True)
    _log(f"Listening on port {_port}")


def _poll():
    """Timer callback - check for incoming connections."""
    if _server_socket is None:
        return None

    try:
        conn, _ = _server_socket.accept()
        conn.settimeout(1.0)
        try:
            data = conn.recv(1024).decode().strip()
            if data == "reload":
                result = _reload_addon()
            elif data == "ping":
                result = "pong"
            else:
                result = f"error: unknown command '{data}'"
            conn.sendall((result + "\n").encode())
        finally:
            conn.close()
    except BlockingIOError:
        pass
    except Exception:
        traceback.print_exc()

    return 0.1  # poll every 100ms


# -- Entry point --

try:
    _create_link()
    _enable_addon()
    _start_server()
    _log("Ready")
except Exception:
    traceback.print_exc()
    _log("Setup failed!")
