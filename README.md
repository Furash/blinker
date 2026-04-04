# blinker

CLI tool for hot-reloading Blender addons during development. Launches Blender with your addon symlinked into the extensions directory and runs a TCP server inside Blender that accepts reload commands. When you send a reload, it disables the addon, purges all its modules from `sys.modules`, and re-enables it — picking up any file changes from disk.

Built as a lightweight alternative to the reload functionality in [Jacques Lucke's Blender Development VS Code extension](https://github.com/JacquesLucke/blender_vscode). Uses the same disable/purge/enable technique but replaces Flask + HTTP + debugpy with a plain TCP socket and `bpy.app.timers`. No dependencies beyond Python stdlib and Blender's `bpy`.

## Requirements

- Blender 4.2+ (uses the extensions system, not legacy `bl_info` addons)
- Python 3.x on the host machine (for running the CLI)
- Windows: requires permission to create junctions (default for most users)

## Setup

Clone the repo and add it to your PATH:

```
git clone https://github.com/Furash/blinker.git D:\git\blinker
setx PATH "%PATH%;D:\git\blinker"
```

Restart your terminal. The `blinker` command should now be available.

Alternatively, run it directly with `python path/to/blinker.py`.

## Usage

### Launch Blender with an addon

```
blinker path/to/your/addon
```

This will:

1. Find a Blender executable (searches `BLENDER_PATH` env var, then `PATH`, then `Program Files`)
2. Check all existing Blender extension repos for a symlink/junction pointing to your addon — if one exists (e.g. from the VS Code extension), it reuses it
3. If no link exists, create a `blinker` extension repository in Blender and junction your addon directory into it
4. Launch Blender with `--python bootstrap.py` which enables the addon and starts a TCP server on `localhost:9876`

Blender runs in the foreground. The TCP server lives for the duration of the Blender process.

```
blinker D:\git\plumbline
```

Output:
```
  addon:   D:\git\plumbline
  module:  bl_ext.vscode_development.plumbline
  blender: C:\Program Files\Blender Foundation\Blender 5.1\blender.exe
  port:    9876
```

### Reload the addon

From another terminal:

```
blinker reload
```

This connects to the TCP server on port 9876, sends `reload`, and prints the result. Exit code is 0 on success, 1 on failure.

The reload sequence (runs on Blender's main thread via `bpy.app.timers`):

1. `bpy.ops.preferences.addon_disable(module=name)`
2. Delete all `sys.modules` entries matching the addon's module prefix
3. `bpy.ops.preferences.addon_enable(module=name)`
4. Redraw all areas

This is the same technique the VS Code Blender Development extension uses in its `UpdateAddonOperator`.

### Options

```
blinker <addon_path> [--blender PATH] [--port PORT] [--repo NAME] [--module NAME] [--blend FILE]
blinker reload [--port PORT]
```

| Flag | Default | Description |
|------|---------|-------------|
| `--blender` | auto-detect | Path to `blender.exe` |
| `--port` | `9876` | TCP port for the reload server |
| `--repo` | `blinker` | Blender extensions repository name (only used if no existing link is found) |
| `--module` | addon folder name | Module name registered in Blender |
| `--blend` | none | `.blend` file to open on launch |

## How it works

### Architecture

```
Terminal 1                              Blender
----------                              -------
blinker D:\git\myaddon
  |  blender.exe --python bootstrap.py
     env: BLINKER_ADDON_PATH=...        bootstrap.py:
           BLINKER_MODULE=...             |  create junction if needed
           BLINKER_REPO=...              |  bpy.ops.preferences.addon_enable()
           BLINKER_PORT=9876             |  start TCP server on :9876
                                               |
Terminal 2                                     |
----------                                     |
blinker reload ---- TCP "reload\n" ----------->|
               <--- "ok (7 modules)\n" --------+
```

### Link detection

On startup, `bootstrap.py` iterates over all enabled Blender extension repositories and checks each directory entry with `os.readlink()`. If an existing junction/symlink points to the same addon source directory, blinker reuses it and adopts that repo's module name. This means blinker coexists with the VS Code extension without creating duplicate links.

Windows junctions return paths with a `\\?\` prefix from `os.readlink()` — the comparison strips this before matching. Directories that aren't reparse points (WinError 4390) are skipped.

### Server

The TCP server is a non-blocking socket polled every 100ms by `bpy.app.timers.register(..., persistent=True)`. It accepts single-line text commands:

| Command | Response |
|---------|----------|
| `reload` | `ok (N modules)` or `error: ...` |
| `ping` | `pong` |

One connection at a time. The timer runs on Blender's main thread, so addon disable/enable calls have full access to `bpy.context`.

## Claude Code integration

Add a Stop hook to auto-reload after every Claude Code response. In your addon project's `.claude/settings.json`:

```json
{
  "hooks": {
    "Stop": [
      {
        "command": "blinker reload",
        "timeout": 3000
      }
    ]
  }
}
```

With this, the workflow is:

1. Run `blinker path/to/addon` in a terminal
2. Open the addon project in Claude Code
3. Ask Claude to make changes
4. Claude edits files, finishes, hook fires, addon reloads in Blender

## Files

| File | Runs where | Purpose |
|------|-----------|---------|
| `blinker.py` | Host machine | CLI entry point — launches Blender or sends reload |
| `bootstrap.py` | Inside Blender | Symlink setup, addon enable, TCP reload server |
| `blinker.cmd` | Host machine | Windows command wrapper (`python blinker.py %*`) |

## Troubleshooting

**"No blinker server on port 9876"** — Blender isn't running or bootstrap.py failed during startup. Check Blender's system console (Window > Toggle System Console) for `[blinker]` messages.

**Addon appears twice in preferences** — A link exists in multiple extension repos (e.g. both `blinker` and `vscode_development`). Remove the duplicate junction manually and delete the extra repo from Blender preferences.

**"error: disable failed"** — The addon's `unregister()` function threw an exception. Check Blender's console for the traceback. The reload is aborted — fix the error and try again.

**Junction creation fails (PermissionError)** — On Windows, junction creation requires the path to not already exist as a real directory. If a previous junction was replaced by a real directory copy (e.g. after a Blender settings import), delete it manually.

## License

GPL-3.0
