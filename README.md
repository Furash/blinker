# blinker

CLI tool for hot-reloading Blender addons during development. Launches Blender with your addon symlinked into the extensions directory and runs a TCP server inside Blender that accepts reload commands. When you send a reload, it disables the addon, purges all its modules from `sys.modules`, and re-enables it — picking up any file changes from disk.

Built as a lightweight alternative to the reload functionality in [Jacques Lucke's Blender Development VS Code extension](https://github.com/JacquesLucke/blender_vscode). Uses the same disable/purge/enable technique but replaces Flask + HTTP + debugpy with a plain TCP socket and `bpy.app.timers`. No dependencies beyond Python stdlib and Blender's `bpy`.

## Requirements

- Blender 4.2+ (uses the extensions system, not legacy `bl_info` addons)
- Python 3.x on the host machine (for running the CLI)
- Windows: requires permission to create junctions (default for most users)
- Linux/macOS: requires permission to create symlinks (default for most users)

## Setup

Clone the repo and add it to your PATH:

**Windows:**
```
git clone https://github.com/Furash/blinker.git D:\git\blinker
setx PATH "%PATH%;D:\git\blinker"
```

**Linux/macOS:**
```
git clone https://github.com/Furash/blinker.git ~/blinker
echo 'export PATH="$HOME/blinker:$PATH"' >> ~/.bashrc
source ~/.bashrc
```

Restart your terminal. The `blinker` command should now be available.

Alternatively, run it directly with `python3 path/to/blinker.py`.

## Usage

### Launch Blender with an addon

```
blinker path/to/your/addon
```

This will:

1. Find a Blender executable (searches `BLENDER_PATH` env var, then `PATH`, then platform-specific locations)
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

1. Set `bpy.app.driver_namespace["_blinker_reloading"] = True`
2. Call the addon's `blinker_pre_reload()` hook if defined
3. `bpy.ops.preferences.addon_disable(module=name)`
4. Delete all `sys.modules` entries matching the addon's module prefix
5. `bpy.ops.preferences.addon_enable(module=name)`
6. Redraw all areas
7. Clear the `_blinker_reloading` flag after a short delay

This is the same core technique the VS Code Blender Development extension uses in its `UpdateAddonOperator`, with the addition of pre-reload hooks for safe cleanup.

### Options

```
blinker <addon_path> [--blender PATH] [--port PORT] [--repo NAME] [--module NAME] [--blend FILE]
blinker reload [--port PORT]
```

| Flag | Default | Description |
|------|---------|-------------|
| `--blender` | auto-detect | Path to Blender executable |
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

Windows junctions return paths with a `\\?\` prefix from `os.readlink()` — the comparison strips this before matching. Directories that aren't reparse points (WinError 4390) are skipped. On Linux/macOS, regular directories are skipped via EINVAL.

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
| `blinker` | Host machine | Linux/macOS shell wrapper (`python3 blinker.py "$@"`) |

## Preparing addons with modal operators for reload

Reloading an addon while a modal operator is running will crash Blender. The reload sequence calls `addon_disable`, which unregisters all operator classes. Any running modal operator still holds a `self` reference to the now-deleted class — the next time Blender calls `modal()` or a draw callback accesses `self`, you get a `ReferenceError` or a hard crash.

This only affects addons that use modal operators (especially with `draw_handler_add`). If your addon doesn't have any, you can skip this section.

### The problem in detail

The crash sequence:

1. Modal operator is running, draw handler is registered
2. `blinker reload` fires
3. `addon_disable` unregisters all classes, including the modal operator's class
4. Blender still has the modal handler queued — it calls `modal()` on the old `self`
5. `self` points to a freed class → crash

Draw handlers are worse: they survive `addon_disable` because `draw_handler_add` doesn't track which addon registered them. The handler keeps a reference to `self`, but `self`'s class no longer exists.

### Solution: pre-reload hook + draw callback guard

Blinker provides two mechanisms. Use both together for modal operators with draw handlers.

#### 1. Pre-reload hook

Define a `blinker_pre_reload()` function in your addon's `__init__.py`. Blinker calls it before `addon_disable`, giving you a chance to cancel active modals and remove draw handlers cleanly.

In your `__init__.py`, track the active modal and define the hook:

```python
_active_modal = None

def blinker_pre_reload():
    if _active_modal is not None:
        _active_modal.cancel_modal()
```

In your modal operator, store a reference on invoke and implement cleanup:

```python
class MYADDON_OT_ModalTool(bpy.types.Operator):
    _draw_handle = None

    def invoke(self, context, event):
        import my_addon
        my_addon._active_modal = self
        self.__class__._draw_handle = bpy.types.SpaceView3D.draw_handler_add(
            self._draw_callback, (context,), 'WINDOW', 'POST_PIXEL')
        context.window_manager.modal_handler_add(self)
        return {'RUNNING_MODAL'}

    def cancel_modal(self):
        if self.__class__._draw_handle is not None:
            bpy.types.SpaceView3D.draw_handler_remove(
                self.__class__._draw_handle, 'WINDOW')
            self.__class__._draw_handle = None
        import my_addon
        my_addon._active_modal = None
```

The hook runs on Blender's main thread (same as the reload), so calling `draw_handler_remove` here is safe.

#### 2. Draw callback guard

Even with the pre-reload hook, there is a small window where Blender may call an orphaned draw handler before the hook runs — or the hook may not cover every draw handler. As a safety net, check the `_blinker_reloading` flag at the top of every draw callback:

```python
def _draw_callback(self, context):
    if bpy.app.driver_namespace.get("_blinker_reloading"):
        return
    # safe to access self here
    self._do_drawing(context)
```

Blinker sets `bpy.app.driver_namespace["_blinker_reloading"]` to `True` before the reload starts and clears it shortly after reload completes. Orphaned draw handlers from the old operator instance see the flag during the first post-reload redraw and return early instead of crashing.

#### When to use which

| Situation | What to use |
|-----------|------------|
| Modal operator, no draw handler | Pre-reload hook only |
| Modal operator + draw handler | Both — hook cancels the modal, guard protects the draw callback |
| Draw handler without a modal (e.g. persistent overlay) | Guard only — remove the handler in your addon's `unregister()` |

## Troubleshooting

**"No blinker server on port 9876"** — Blender isn't running or bootstrap.py failed during startup. Check Blender's system console (Window > Toggle System Console) for `[blinker]` messages.

**Addon appears twice in preferences** — A link exists in multiple extension repos (e.g. both `blinker` and `vscode_development`). Remove the duplicate junction manually and delete the extra repo from Blender preferences.

**"error: disable failed"** — The addon's `unregister()` function threw an exception. Check Blender's console for the traceback. The reload is aborted — fix the error and try again.

**Junction/symlink creation fails (PermissionError)** — On Windows, junction creation requires the path to not already exist as a real directory. On Linux, symlink creation requires the link path to not already exist. If a previous link was replaced by a real directory copy (e.g. after a Blender settings import), delete it manually.

## License

GPL-3.0
