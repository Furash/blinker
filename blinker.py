#!/usr/bin/env python3
"""Blinker - Blender addon hot-reload CLI.

Usage:
    blinker <addon_path> [options]    Launch Blender with addon + reload server
    blinker reload [--port PORT]      Reload addon in running Blender
"""

import os
import shutil
import socket
import subprocess
import sys
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

    blender = args.blender or find_blender()
    if not blender:
        print("Error: Blender not found. Use --blender or set BLENDER_PATH")
        return 1

    module = args.module or addon_path.name

    print(f"  addon:   {addon_path}")
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
    }

    cmd = [blender, "--python", str(BOOTSTRAP)]
    if args.blend:
        cmd.append(args.blend)

    try:
        return subprocess.call(cmd, env=env)
    except KeyboardInterrupt:
        return 0


def cmd_reload(argv):
    import argparse

    p = argparse.ArgumentParser(prog="blinker reload")
    p.add_argument("--port", type=int, default=DEFAULT_PORT)
    args = p.parse_args(argv)

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


def main():
    if len(sys.argv) < 2 or sys.argv[1] in ("-h", "--help"):
        print(__doc__.strip())
        return 0

    if sys.argv[1] == "reload":
        return cmd_reload(sys.argv[2:])
    else:
        return cmd_start(sys.argv[1:])


if __name__ == "__main__":
    sys.exit(main() or 0)
