"""Render the PySide6 Blinker UI window to PNG files without showing it on screen.

Reference screenshots for the Tauri rewrite (visual parity). The window is rendered
with Qt.WA_DontShowOnScreen (real Windows fonts, never mapped on screen; the `offscreen`
platform has no font database), DPI-unaware so 1 logical px = 1 image px, with a
throw-away config in a temp dir. It never starts Blender, probes ports, or writes the
real config.

    python -I tools/grab_reference.py <out_dir> [variant-name-prefix]

Runs one child process per variant (QT_SCALE_FACTOR must be set before QApplication).
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# Shown in the toolbar label instead of the temp path, so the mock UI can show the same.
FAKE_CONFIG_LABEL = r"C:\Users\user\AppData\Roaming\BlinkerUI\blinker_ui_config.json"

FOLDERS = [
    {"path": "D:/git/Blender_CCP_Tools", "port": 9876, "repo": "blinker", "module": "", "blend": "",
     "favourite": True},
    {"path": "D:/git/blender_addons/ccp_exporter", "port": 9877, "repo": "user_default",
     "module": "ccp_exporter", "blend": "D:/scenes/export_test.blend", "favourite": False},
    {"path": "D:/git/blender_addons/quick_tools", "port": 9878, "repo": "blinker", "module": "",
     "blend": "", "favourite": False},
]

SAMPLE_OUTPUT = (
    "$ C:/Program Files/Blender Foundation/Blender 4.5/blender.exe --python bootstrap.py\n"
    "  addon:  D:\\git\\blender_addons\\ccp_exporter\n"
    "  module: bl_ext.user_default.ccp_exporter\n"
    "  port:   9877\n\n"
    "Blender 4.5.3 LTS (hash 67807e1800cc built 2025-09-09 01:31:16)\n"
    "[blinker] bootstrap server listening on 127.0.0.1:9877\n"
    "[blinker] enabled bl_ext.user_default.ccp_exporter\n"
    "[reload] ok\n"
)

# variant name -> (theme preset, scale, state, logical width). The 1100 px window is
# narrower than a row (the list scrolls horizontally), so "wide" variants show whole rows.
VARIANTS = {
    "tokyo_state_100": ("Tokyo Night (default)", "1", "state", 1100),
    "tokyo_plain_100": ("Tokyo Night (default)", "1", "plain", 1100),
    "tokyo_empty_100": ("Tokyo Night (default)", "1", "empty", 1100),
    "frigus_state_100": ("Frigus Nox", "1", "state", 1100),
    "light_state_100": ("Light", "1", "state", 1100),
    "tokyo_state_150": ("Tokyo Night (default)", "1.5", "state", 1100),
    "frigus_state_150": ("Frigus Nox", "1.5", "state", 1100),
    "light_state_150": ("Light", "1.5", "state", 1100),
    "tokyo_wide_100": ("Tokyo Night (default)", "1", "state", 1700),
    "frigus_wide_100": ("Frigus Nox", "1", "state", 1700),
    "light_wide_100": ("Light", "1", "state", 1700),
    "tokyo_wide_150": ("Tokyo Night (default)", "1.5", "state", 1700),
    # Dialogs (Task 10): the dialog's client area only (no native title bar), over the
    # main window's stylesheet. Width is unused.
    "dlg_scan_tokyo_100": ("Tokyo Night (default)", "1", "dlg_scan", 1100),
    "dlg_scan_light_100": ("Light", "1", "dlg_scan", 1100),
    "dlg_edit_tokyo_100": ("Tokyo Night (default)", "1", "dlg_edit", 1100),
    "dlg_edit_light_100": ("Light", "1", "dlg_edit", 1100),
    "dlg_noaddons_tokyo_100": ("Tokyo Night (default)", "1", "dlg_noaddons", 1100),
    "dlg_noaddons_light_100": ("Light", "1", "dlg_noaddons", 1100),
    # Settings dialog (Task 11), one capture per tab.
    "dlg_settings_terminal_tokyo_100": ("Tokyo Night (default)", "1", "dlg_settings_terminal", 1100),
    "dlg_settings_terminal_light_100": ("Light", "1", "dlg_settings_terminal", 1100),
    "dlg_settings_fonts_tokyo_100": ("Tokyo Night (default)", "1", "dlg_settings_fonts", 1100),
    "dlg_settings_fonts_light_100": ("Light", "1", "dlg_settings_fonts", 1100),
    "dlg_settings_colors_tokyo_100": ("Tokyo Night (default)", "1", "dlg_settings_colors", 1100),
    "dlg_settings_colors_light_100": ("Light", "1", "dlg_settings_colors", 1100),
}

SETTINGS_TABS = {"dlg_settings_terminal": 0, "dlg_settings_fonts": 1, "dlg_settings_colors": 2}

# Scan dialog content (mock.js uses the same): root, then (path, kind) as find_addons
# returns them; ccp_exporter is already in FOLDERS.
SCAN_ROOT = r"D:\git\blender_addons"
SCAN_FOUND = [
    (r"D:\git\blender_addons\asset_tools", "extension"),
    (r"D:\git\blender_addons\ccp_exporter", "extension"),
    (r"D:\git\blender_addons\legacy\mesh_cleanup", "legacy"),
    (r"D:\git\blender_addons\quick_tools_v2", "extension"),
    (r"D:\git\blender_addons\uv_helpers", "legacy"),
]


def build_dialog(b, w, kind: str):
    """The dialog for a `dlg_*` state, built exactly as MainWindow builds it."""
    from pathlib import Path as P

    from PySide6.QtWidgets import QMessageBox, QTabWidget

    if kind in SETTINGS_TABS:
        # MainWindow._open_settings, without exec()
        dlg = b.SettingsDialog(w, w.theme, w.terminal, w.ai_aliases, w.blender_path)
        dlg.findChild(QTabWidget).setCurrentIndex(SETTINGS_TABS[kind])
        return dlg
    if kind == "dlg_scan":
        existing = {str(P(f.path).resolve()) for f in w.folders}
        found = [(P(p), k) for p, k in SCAN_FOUND]
        return b.ScanAddonsDialog(w, P(SCAN_ROOT), found, existing)
    if kind == "dlg_edit":
        return b.EditFolderDialog(w.folders[1], w)
    # QMessageBox.information(self, "No addons", ...) without exec()
    return QMessageBox(QMessageBox.Icon.Information, "No addons",
                       f"No Blender addons or extensions found under:\n{P(SCAN_ROOT)}",
                       QMessageBox.StandardButton.Ok, w)


def render(out: Path, preset: str, state: str, width: int) -> None:
    sys.path.insert(0, str(REPO))
    import blinker_ui as b
    from PySide6.QtCore import Qt, QTimer
    from PySide6.QtWidgets import QApplication, QLabel

    tmp = Path(tempfile.mkdtemp(prefix="blinker_ref_"))
    cfg_path = tmp / "blinker_ui_config.json"
    cfg = {
        "folders": [] if state == "empty" else FOLDERS,
        "ai_aliases": "claude, codex",
        "theme": dict(b.BUILTIN_THEMES[preset]),
    }
    cfg_path.write_text(json.dumps(cfg), encoding="utf-8")

    # Never touch the real config, never probe ports or spawn anything.
    b.CONFIG = cfg_path
    b.save_config = lambda c: cfg_path.write_text(json.dumps(c), encoding="utf-8")
    b.MainWindow._poll_status = lambda self: None
    b.is_running = lambda port: False

    app = QApplication(sys.argv)
    app.setApplicationName("Blinker UI")
    app.setStyle("Fusion")
    w = b.MainWindow()
    w.poll_timer.stop()
    for lbl in w.findChildren(QLabel):
        if lbl.text().startswith("Config: "):
            lbl.setText(f"Config: {FAKE_CONFIG_LABEL}")
    w.setAttribute(Qt.WidgetAttribute.WA_DontShowOnScreen, True)
    w.resize(width, 820)
    w.show()

    def set_state() -> None:
        if state == "state":
            # row 0 running, row 1 selected with output
            w.folders[0].running = True
            w.rows[0].update_state()
            w.folders[1].output = [SAMPLE_OUTPUT]
            w._select(1)

    dlg = None
    if state.startswith("dlg_"):
        dlg = build_dialog(b, w, state)
        dlg.setAttribute(Qt.WidgetAttribute.WA_DontShowOnScreen, True)
        dlg.show()

    def grab() -> None:
        (dlg or w).grab().save(str(out))
        if dlg is not None:
            dlg.close()
        w.close()
        app.quit()

    # Run the real event loop so deleteLater/layout/polish all settle before grabbing.
    QTimer.singleShot(300, set_state)
    QTimer.singleShot(1000, grab)
    app.exec()


def main() -> None:
    if len(sys.argv) >= 6 and sys.argv[1] == "--child":
        render(Path(sys.argv[2]), sys.argv[3], sys.argv[4], int(sys.argv[5]))
        return
    out_dir = Path(sys.argv[1]) if len(sys.argv) > 1 else REPO / ".superpowers" / "ref"
    only = sys.argv[2] if len(sys.argv) > 2 else ""  # optional variant name prefix
    out_dir.mkdir(parents=True, exist_ok=True)
    for name, (preset, scale, state, width) in VARIANTS.items():
        if not name.startswith(only):
            continue
        env = dict(os.environ, QT_QPA_PLATFORM="windows:dpiawareness=0", QT_SCALE_FACTOR=scale)
        out = out_dir / f"ref_{name}.png"
        subprocess.run([sys.executable, "-I", __file__, "--child", str(out), preset, state, str(width)],
                       env=env, check=True)
        print(out)


if __name__ == "__main__":
    main()
