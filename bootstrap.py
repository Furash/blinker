"""Blinker bootstrap - runs inside Blender via --python.

Sets up addon symlink, enables addon, starts TCP reload server.
"""

import os
import socket
import sys
import tempfile
import traceback
from pathlib import Path

import bpy

_addon_path = Path(os.environ["BLINKER_ADDON_PATH"])
_module = os.environ["BLINKER_MODULE"]
_repo = os.environ["BLINKER_REPO"]
_port = int(os.environ["BLINKER_PORT"])
_legacy = os.environ.get("BLINKER_LEGACY") == "1"
_full_module = _module if _legacy else f"bl_ext.{_repo}.{_module}"

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
        # EINVAL = not a symlink (Linux/macOS)
        import errno
        if e.errno == errno.EINVAL:
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


def _get_legacy_addons_dir():
    """Return the user scripts/addons directory for legacy addons."""
    return Path(bpy.utils.user_resource('SCRIPTS', path="addons"))


def _find_existing_link():
    """Search addon directories for an existing link to _addon_path."""
    resolved = _addon_path.resolve()
    _log(f"Looking for existing link to {resolved}")

    if _legacy:
        # Search legacy addons directory
        addons_dir = _get_legacy_addons_dir()
        if addons_dir.is_dir():
            for entry in addons_dir.iterdir():
                if not entry.is_dir():
                    continue
                target = _resolve_link(entry)
                if target is not None:
                    _log(f"  {entry.name} in addons/ -> {target}")
                    if target == resolved:
                        return entry.name, entry
        return None, None

    # Search extension repos
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


def _make_link(link_path):
    """Create a symlink (Unix) or junction (Windows) from link_path to _addon_path."""
    if link_path.exists() or link_path.is_symlink():
        _log(f"Removing old link: {link_path}")
        os.remove(link_path)

    if sys.platform == "win32":
        import _winapi
        _winapi.CreateJunction(str(_addon_path), str(link_path))
    else:
        os.symlink(str(_addon_path), str(link_path), target_is_directory=True)

    _log(f"Linked: {link_path} -> {_addon_path}")


def _create_link():
    global _full_module

    existing_id, existing_path = _find_existing_link()
    if existing_path is not None:
        if _legacy:
            _full_module = existing_path.name
        else:
            _full_module = f"bl_ext.{existing_id}.{existing_path.name}"
        _log(f"Found existing link: {existing_path}")
        return

    if _legacy:
        # Link into scripts/addons/
        addons_dir = _get_legacy_addons_dir()
        os.makedirs(addons_dir, exist_ok=True)
        _make_link(addons_dir / _module)
    else:
        # Link into extensions repo
        repo = _ensure_repo()
        repo_dir = Path(repo.custom_directory if repo.use_custom_directory else repo.directory)
        os.makedirs(repo_dir, exist_ok=True)
        _make_link(repo_dir / _module)


# -- Addon enable / reload --


def _enable_addon():
    if _legacy:
        bpy.ops.preferences.addon_refresh()
    else:
        bpy.ops.extensions.repo_refresh_all()
    bpy.ops.preferences.addon_enable(module=_full_module)
    _log(f"Enabled: {_full_module}")


def _notify_pre_reload():
    """Call addon's blinker_pre_reload() hook if defined.

    Addons can define this function to clean up state that can't survive
    a reload, such as cancelling modal operators and removing draw handlers.
    """
    try:
        mod = sys.modules.get(_full_module)
        if mod is None:
            return
        hook = getattr(mod, "blinker_pre_reload", None)
        if hook is not None:
            _log("Calling blinker_pre_reload()")
            hook()
    except Exception:
        _log("Error in blinker_pre_reload():")
        traceback.print_exc()


def _reload_addon():
    """Disable -> purge sys.modules -> re-enable. Same technique as VS Code extension."""
    # Set flag so draw callbacks can detect reload and bail out before
    # accessing self (which becomes invalid after class unregistration).
    # Checked via: bpy.app.driver_namespace.get("_blinker_reloading")
    bpy.app.driver_namespace["_blinker_reloading"] = True

    _notify_pre_reload()

    try:
        bpy.ops.preferences.addon_disable(module=_full_module)
    except Exception:
        traceback.print_exc()
        bpy.app.driver_namespace.pop("_blinker_reloading", None)
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
        bpy.app.driver_namespace.pop("_blinker_reloading", None)
        return "error: enable failed"

    for window in bpy.context.window_manager.windows:
        for area in window.screen.areas:
            area.tag_redraw()

    # Clear the flag after a short delay so orphaned draw handlers from the
    # old modal operator still see it during the first redraw after reload.
    def _clear_reloading_flag():
        bpy.app.driver_namespace.pop("_blinker_reloading", None)
        return None
    bpy.app.timers.register(_clear_reloading_flag, first_interval=0.2)

    _log(f"Reloaded {_full_module} ({count} modules)")
    return f"ok ({count} modules)"


def _prepare_restart(mode=None):
    """Save state (if requested) and schedule Blender exit for restart."""
    marker = os.path.join(tempfile.gettempdir(), "blinker_restart_path")

    if mode == "save":
        if bpy.data.filepath:
            try:
                bpy.ops.wm.save_mainfile()
                with open(marker, "w") as f:
                    f.write(bpy.data.filepath)
                _log(f"Saved {bpy.data.filepath}")
            except Exception:
                _log("Could not save file for restart")
                traceback.print_exc()
        else:
            mode = "temp"

    if mode == "temp":
        blend_path = os.path.join(tempfile.gettempdir(), "blinker_restart.blend")
        try:
            bpy.ops.wm.save_as_mainfile(filepath=blend_path, copy=True)
            with open(marker, "w") as f:
                f.write(blend_path)
        except Exception:
            _log("Could not save scene for restart")
            traceback.print_exc()

    def _quit():
        os._exit(75)
    bpy.app.timers.register(_quit, first_interval=0.2)

    _log("Restarting...")
    return "ok"


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
    except BlockingIOError:
        return 0.1
    except Exception:
        traceback.print_exc()
        return 0.1

    try:
        conn.settimeout(1.0)
        try:
            data = conn.recv(1024).decode().strip()
        except (ConnectionResetError, ConnectionAbortedError, socket.timeout):
            return 0.1  # client gave up before we read; common with idle-timer ping probes
        if data == "reload":
            result = _reload_addon()
        elif data.startswith("restart"):
            parts = data.split(None, 1)
            result = _prepare_restart(parts[1] if len(parts) > 1 else None)
        elif data == "kill":
            _log("Kill requested")
            def _quit_now():
                os._exit(0)
            bpy.app.timers.register(_quit_now, first_interval=0.1)
            result = "ok"
        elif data == "ping":
            result = f"pong\t{_full_module}\t{_addon_path}"
        else:
            result = f"error: unknown command '{data}'"
        try:
            conn.sendall((result + "\n").encode())
        except (ConnectionResetError, ConnectionAbortedError):
            pass
    except Exception:
        traceback.print_exc()
    finally:
        try:
            conn.close()
        except Exception:
            pass

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
