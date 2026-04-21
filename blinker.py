#!/usr/bin/env python3
"""Blinker - Blender addon hot-reload CLI.

Usage:
    blinker <addon_path> [options]         Launch Blender with addon + reload server
    blinker list                           List running instances with indices
    blinker reload  [INDEX] [--port PORT]  Reload addon in running Blender
    blinker restart [INDEX] [--port PORT]  Restart Blender (clears console, --no-clear to keep)
"""

import os
import shutil
import socket
import subprocess
import sys
import tempfile
from pathlib import Path

DEFAULT_PORT = 9876
SCAN_PORTS = range(9876, 9896)
BOOTSTRAP = Path(__file__).resolve().parent / "bootstrap.py"


def _probe(port, timeout=0.3):
    """Connect to port, send ping. Return (module, addon_path) or None."""
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=timeout) as sock:
            sock.sendall(b"ping\n")
            resp = sock.recv(1024).decode().strip()
    except (ConnectionRefusedError, socket.timeout, OSError):
        return None
    if not resp.startswith("pong"):
        return None
    parts = resp.split("\t")
    module = parts[1] if len(parts) > 1 else "?"
    addon = parts[2] if len(parts) > 2 else "?"
    return module, addon


def _scan():
    """Return list of (port, module, addon_path) for running instances.

    Probes all ports in parallel — on Windows, connects to closed ports hit
    the full timeout instead of failing instantly, so sequential scans are
    unusably slow.
    """
    from concurrent.futures import ThreadPoolExecutor

    with ThreadPoolExecutor(max_workers=len(SCAN_PORTS)) as ex:
        results = list(ex.map(_probe, SCAN_PORTS))
    return [(port, *info) for port, info in zip(SCAN_PORTS, results) if info is not None]


def _resolve_port(index_or_port, scan_if_index=True):
    """Turn a CLI argument into a port number.

    None -> DEFAULT_PORT. Small integer (1..99) treated as 1-based list index
    and resolved via _scan(). Larger integer treated as a port number directly.
    """
    if index_or_port is None:
        return DEFAULT_PORT
    n = int(index_or_port)
    if n >= 100:
        return n
    if not scan_if_index:
        return n
    instances = _scan()
    if not instances:
        print("No running blinker instances")
        return None
    if n < 1 or n > len(instances):
        print(f"Index {n} out of range (found {len(instances)} instances)")
        return None
    return instances[n - 1][0]


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
    p.add_argument("index", nargs="?", help="Instance index from `blinker list` (1-based)")
    p.add_argument("--port", type=int, help="Target port (overrides index)")
    p.add_argument("--clear", action="store_true", help="Clear console before reload")
    args = p.parse_args(argv)

    port = args.port if args.port is not None else _resolve_port(args.index)
    if port is None:
        return 1

    if args.clear:
        os.system("cls" if sys.platform == "win32" else "clear")

    try:
        with socket.create_connection(("127.0.0.1", port), timeout=3) as sock:
            sock.sendall(b"reload\n")
            resp = sock.recv(1024).decode().strip()
            print(resp)
            return 0 if resp.startswith("ok") else 1
    except ConnectionRefusedError:
        print(f"No blinker server on port {port}")
        return 1
    except socket.timeout:
        print(f"Timeout connecting to port {port}")
        return 1


def cmd_restart(argv):
    import argparse

    p = argparse.ArgumentParser(prog="blinker restart")
    p.add_argument("index", nargs="?", help="Instance index from `blinker list` (1-based)")
    p.add_argument("--port", type=int, help="Target port (overrides index)")
    p.add_argument("--no-clear", action="store_true", help="Don't clear console before restart")
    p.add_argument("--save", action="store_true", help="Save current .blend before restarting")
    p.add_argument("--temp", action="store_true", help="Save scene to temp file before restarting")
    args = p.parse_args(argv)

    port = args.port if args.port is not None else _resolve_port(args.index)
    if port is None:
        return 1

    if not args.no_clear:
        os.system("cls" if sys.platform == "win32" else "clear")

    command = "restart"
    if args.save:
        command = "restart save"
    elif args.temp:
        command = "restart temp"

    try:
        with socket.create_connection(("127.0.0.1", port), timeout=3) as sock:
            sock.sendall((command + "\n").encode())
            resp = sock.recv(1024).decode().strip()
            print(resp)
            return 0 if resp.startswith("ok") else 1
    except ConnectionRefusedError:
        print(f"No blinker server on port {port}")
        return 1
    except socket.timeout:
        print(f"Timeout connecting to port {port}")
        return 1


def cmd_list(argv):
    instances = _scan()
    if not instances:
        print("No running blinker instances")
        return 0
    idx_w = len(str(len(instances)))
    mod_w = max(len(m) for _, m, _ in instances)
    for i, (port, module, addon) in enumerate(instances, start=1):
        print(f"  {i:>{idx_w}}  {port}  {module:<{mod_w}}  {addon}")
    return 0


def main():
    if len(sys.argv) < 2 or sys.argv[1] in ("-h", "--help"):
        print(__doc__.strip())
        return 0

    if sys.argv[1] == "reload":
        return cmd_reload(sys.argv[2:])
    elif sys.argv[1] == "restart":
        return cmd_restart(sys.argv[2:])
    elif sys.argv[1] == "list":
        return cmd_list(sys.argv[2:])
    else:
        return cmd_start(sys.argv[1:])


if __name__ == "__main__":
    sys.exit(main() or 0)
