#!/usr/bin/env python3
"""Blinker - Blender addon hot-reload CLI.

Usage:
    blinker <addon_path> [options]    Launch Blender with addon + reload server
    blinker reload [--port PORT]      Reload addon in running Blender
    blinker restart [--port PORT]     Restart Blender (clears console, --no-clear to keep)
"""

import os
import shutil
import socket
import subprocess
import sys
import tempfile
from pathlib import Path

DEFAULT_PORT = 9876
BOOTSTRAP = Path(__file__).resolve().parent / "bootstrap.py"


def find_blender():
    """Search for blender executable."""
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
        # Common Linux / macOS install locations
        for candidate in (
            "/snap/bin/blender",
            "/usr/bin/blender",
            "/usr/local/bin/blender",
            Path.home() / "blender" / "blender",
            # Flatpak — binary is available at this path when installed
            "/var/lib/flatpak/exports/bin/org.blender.Blender",
        ):
            if Path(candidate).is_file():
                return str(candidate)

    return None


def cmd_start(argv):
    import argparse

    p = argparse.ArgumentParser(prog="blinker", description="Launch Blender with addon")
    p.add_argument("addon", help="Path to addon directory")
    p.add_argument("--blender", help="Blender executable path")
    p.add_argument("--port", type=int, default=DEFAULT_PORT)
    p.add_argument("--repo", default="blinker", help="Extensions repo name (default: blinker)")
    p.add_argument("--module", help="Module name (default: addon folder name)")
    p.add_argument("--blend", help=".blend file to open")
    args = p.parse_args(argv)

    addon_path = Path(args.addon).resolve()
    if not addon_path.is_dir():
        print(f"Error: {addon_path} is not a directory")
        return 1
    if not (addon_path / "blender_manifest.toml").exists() and not (addon_path / "__init__.py").exists():
        print(f"Error: {addon_path} doesn't look like a Blender addon")
        return 1

    legacy = not (addon_path / "blender_manifest.toml").exists()

    blender = args.blender or find_blender()
    if not blender:
        print("Error: Blender not found. Use --blender or set BLENDER_PATH")
        return 1

    module = args.module or addon_path.name

    print(f"  addon:   {addon_path}")
    if legacy:
        print(f"  module:  {module}  (legacy bl_info addon)")
    else:
        print(f"  module:  bl_ext.{args.repo}.{module}")
    print(f"  blender: {blender}")
    print(f"  port:    {args.port}")
    print()

    env = {
        **os.environ,
        "BLINKER_ADDON_PATH": str(addon_path),
        "BLINKER_MODULE": module,
        "BLINKER_REPO": args.repo,
        "BLINKER_PORT": str(args.port),
        "BLINKER_LEGACY": "1" if legacy else "",
    }

    restart_marker = os.path.join(tempfile.gettempdir(), "blinker_restart_path")
    restart_blend = os.path.join(tempfile.gettempdir(), "blinker_restart.blend")
    blend_file = args.blend

    while True:
        cmd = [blender, "--python", str(BOOTSTRAP)]
        if blend_file:
            cmd.append(blend_file)

        try:
            code = subprocess.call(cmd, env=env)
        except KeyboardInterrupt:
            code = 0

        if code != 75:
            break

        if os.path.isfile(restart_marker):
            with open(restart_marker) as f:
                blend_file = f.read().strip() or args.blend
            os.remove(restart_marker)
        else:
            blend_file = args.blend

        print("\nRestarting Blender...")

    for path in (restart_marker, restart_blend):
        try:
            os.remove(path)
        except FileNotFoundError:
            pass

    return code


def cmd_reload(argv):
    import argparse

    p = argparse.ArgumentParser(prog="blinker reload")
    p.add_argument("--port", type=int, default=DEFAULT_PORT)
    p.add_argument("--clear", action="store_true", help="Clear console before reload")
    args = p.parse_args(argv)

    if args.clear:
        os.system("cls" if sys.platform == "win32" else "clear")

    try:
        with socket.create_connection(("127.0.0.1", args.port), timeout=3) as sock:
            sock.sendall(b"reload\n")
            resp = sock.recv(1024).decode().strip()
            print(resp)
            return 0 if resp.startswith("ok") else 1
    except ConnectionRefusedError:
        print(f"No blinker server on port {args.port}")
        return 1
    except socket.timeout:
        print(f"Timeout connecting to port {args.port}")
        return 1


def cmd_restart(argv):
    import argparse

    p = argparse.ArgumentParser(prog="blinker restart")
    p.add_argument("--port", type=int, default=DEFAULT_PORT)
    p.add_argument("--no-clear", action="store_true", help="Don't clear console before restart")
    p.add_argument("--save", action="store_true", help="Save current .blend before restarting")
    p.add_argument("--temp", action="store_true", help="Save scene to temp file before restarting")
    args = p.parse_args(argv)

    if not args.no_clear:
        os.system("cls" if sys.platform == "win32" else "clear")

    command = "restart"
    if args.save:
        command = "restart save"
    elif args.temp:
        command = "restart temp"

    try:
        with socket.create_connection(("127.0.0.1", args.port), timeout=3) as sock:
            sock.sendall((command + "\n").encode())
            resp = sock.recv(1024).decode().strip()
            print(resp)
            return 0 if resp.startswith("ok") else 1
    except ConnectionRefusedError:
        print(f"No blinker server on port {args.port}")
        return 1
    except socket.timeout:
        print(f"Timeout connecting to port {args.port}")
        return 1


def main():
    if len(sys.argv) < 2 or sys.argv[1] in ("-h", "--help"):
        print(__doc__.strip())
        return 0

    if sys.argv[1] == "reload":
        return cmd_reload(sys.argv[2:])
    elif sys.argv[1] == "restart":
        return cmd_restart(sys.argv[2:])
    else:
        return cmd_start(sys.argv[1:])


if __name__ == "__main__":
    sys.exit(main() or 0)
