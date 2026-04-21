# blinker

Hot-reload CLI for Blender addon development. Symlinks your addon into Blender's addon directory, launches Blender with a TCP reload server, and accepts `reload` commands that disable/purge/re-enable the addon. Supports both legacy addons (`bl_info`) and modern extensions (`blender_manifest.toml`).

Same disable/purge/enable technique as [Blender Development for VS Code](https://github.com/JacquesLucke/blender_vscode), but with a plain TCP socket instead of Flask + HTTP + debugpy. No dependencies beyond stdlib and `bpy`.

## Requirements

- Blender 2.80+ for legacy addons (`bl_info`), Blender 4.2+ for extensions (`blender_manifest.toml`)
- Python 3.x on the host
- Windows: permission to create junctions (default)
- Linux/macOS: permission to create symlinks (default)

## Setup

Clone and add to PATH:

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

Or run directly: `python3 path/to/blinker.py`.

## Usage

```
blinker <addon_path> [--blender PATH] [--port PORT] [--repo NAME] [--module NAME] [--blend FILE]
blinker list
blinker reload  [INDEX] [--port PORT] [--clear]
blinker restart [INDEX] [--port PORT] [--no-clear] [--save | --temp]
```

`blinker path/to/addon` finds Blender (`BLENDER_PATH` env, then `PATH`, then platform-specific locations), symlinks/junctions the addon into Blender, and launches the reload server on `localhost:9876`. Auto-detects addon type: if `blender_manifest.toml` exists, it links into an extensions repo; otherwise it links into `scripts/addons/` as a legacy addon. If an existing link to the same addon exists (e.g. from the VS Code extension), it reuses it.

`blinker list` probes ports 9876–9895 and prints running instances with 1-based indices, module name, and addon path.

`blinker reload` connects to the server and triggers: set `_blinker_reloading` flag → call `blinker_pre_reload()` hook → `addon_disable` → purge `sys.modules` → `addon_enable` → redraw → clear flag.

`blinker restart` closes Blender and relaunches with the same addon. `--save` saves the current `.blend` file in place before restarting (falls back to `--temp` for untitled files). `--temp` saves the scene to a temp file so the restart preserves your scene without touching your real file. Without either flag, Blender restarts with no scene preservation.

`INDEX` selects an instance from `blinker list` (e.g. `blinker reload 2`). Without it, defaults to port 9876. `--port` overrides both. `--repo` defaults to `blinker`. `--module` defaults to the addon folder name. `--blend` opens a `.blend` file on launch. `--clear` clears the console before reloading. Restart clears the console by default; pass `--no-clear` to keep it.

## Running multiple instances in parallel

Useful for developing several branches simultaneously. Use `git worktree` to get separate working copies, then launch each with a distinct `--port` and `--repo` (extensions) or `--module` (legacy):

```
git worktree add ../myaddon-feat feat/uvs
blinker ../myaddon      --port 9876 --repo main
blinker ../myaddon-feat --port 9877 --repo feat

blinker list
  1  9876  bl_ext.main.myaddon  D:\git\myaddon
  2  9877  bl_ext.feat.myaddon  D:\git\myaddon-feat

blinker reload 2
```

`--repo` is Blender's extensions-repo name (unrelated to git) — used purely to keep the two addons from colliding inside Blender. For legacy addons without `blender_manifest.toml`, use `--module name` instead.

## How it works

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
                                               |
blinker restart --- TCP "restart [save|temp]\n" ->|
               <--- "ok\n" ---   save (optional) |
                              +  exit code 75    |
                                               |
Terminal 1 detects exit 75, relaunches Blender
```

`bootstrap.py` scans all enabled extension repos with `os.readlink()` to find existing links to the addon. Windows junctions return `\\?\`-prefixed paths (stripped before comparison); non-link directories are skipped via WinError 4390 (Windows) or EINVAL (Linux).

The TCP server is a non-blocking socket polled every 100ms via `bpy.app.timers`. Commands: `reload` → `ok (N modules)` / `error: ...`, `restart [save|temp]` → `ok` (optionally saves scene, exits with code 75), `ping` → `pong`. Runs on Blender's main thread so disable/enable have full `bpy.context` access.

## Modal operators and draw handlers

Reloading while a modal operator is active crashes Blender — `addon_disable` unregisters the operator class, but Blender still calls `modal()` / draw callbacks on the stale `self`. Draw handlers are worse: `draw_handler_add` doesn't track which addon owns them, so they survive `addon_disable`.

### Pre-reload hook

Define `blinker_pre_reload()` in your addon's `__init__.py`. Called before `addon_disable` on the main thread.

```python
# __init__.py
_active_modal = None

def blinker_pre_reload():
    if _active_modal is not None:
        _active_modal.cancel_modal()
```

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

### Reloading flag

`bpy.app.driver_namespace["_blinker_reloading"]` is `True` for the duration of the reload sequence. Guard draw callbacks with it as a safety net for orphaned handlers:

```python
def _draw_callback(self, context):
    if bpy.app.driver_namespace.get("_blinker_reloading"):
        return
    self._do_drawing(context)
```

The flag clears ~200ms after reload completes so orphaned handlers from the old instance bail out on the first post-reload redraw.

Use both mechanisms together for modal operators with draw handlers. For persistent overlays without a modal, the guard alone is sufficient — clean up the handler in `unregister()`.

## Claude Code integration

Auto-reload on every Claude Code response via a Stop hook in `.claude/settings.json`:

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

## Troubleshooting

**"No blinker server on port 9876"** — Blender isn't running or `bootstrap.py` failed. Check Blender's console for `[blinker]` messages.

**Addon appears twice in preferences** — Link exists in multiple repos. Remove the duplicate junction and delete the extra repo in preferences.

**"error: disable failed"** — `unregister()` threw. Check the traceback in Blender's console. Reload is aborted.

**Junction/symlink creation fails** — Target path already exists as a real directory (e.g. from a Blender settings import). Delete it manually.

## License

GPL-3.0
