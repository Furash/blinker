// SettingsDialog (blinker_ui.py:1835-2212): tabs Terminal, Fonts, Colors; Restore
// Defaults, OK, Cancel, Apply.
//
// Resolves to { theme, terminal, ai_aliases, blender_path } on OK (`_commit_inputs`),
// null on Cancel/Esc/close. Apply calls `onApply(result)` and keeps the dialog open.
// Live preview (not in Python, which shows changes on Apply only): every draft change
// is applied to the page with `preview(theme)`; the caller restores its committed theme
// when the dialog is cancelled.

import { openDialog, pushButton, message } from "./dialog.js";
import { spinBox } from "./rows.js";
import { comboBox } from "./combo.js";
import { colorsTab } from "./colors.js";
import { invoke, pickFile, pickSaveFile } from "./api.js";
import { commitSettings, importNotes, FONT_SIZE_MIN, FONT_SIZE_MAX } from "./logic.js";

const el = (tag, cls, text) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
};

function lineEdit(value, placeholder = "") {
  const i = el("input", "qline");
  i.type = "text";
  i.spellcheck = false;
  i.autocomplete = "off";
  i.value = value ?? "";
  if (placeholder) i.placeholder = placeholder;
  return i;
}

const errText = (e) => (e && typeof e === "object" ? e.message ?? JSON.stringify(e) : String(e));

let fontCache = null; // QFontDatabase.families(), fetched once

/**
 *   cur       { theme, terminal, ai_aliases, blender_path } (the committed values)
 *   info      { presets: [{name, theme}], colorKeys, defaults: { theme, terminal, ai_aliases } }
 *   onApply(result), preview(theme); tab = the tab shown first (Python: Terminal)
 */
export function settingsDialog(cur, info, { onApply, preview, tab = 0 }) {
  const { presets, colorKeys, defaults } = info;
  const draft = { ...cur.theme };

  // Every action that reads or writes the draft runs in order (the backend helpers are
  // async), so OK/Apply always see the result of an edit committed just before.
  let chain = Promise.resolve();
  const run = (fn) => (chain = chain.then(fn).catch((e) => console.error(e)));
  let busy = false; // a native picker or a message box is open

  // ---------- Terminal ----------
  const blenderE = lineEdit(cur.blender_path, "(auto-detect: BLENDER_PATH, PATH, then standard install dirs)");
  const terminalE = lineEdit(cur.terminal);
  const aiE = lineEdit(cur.ai_aliases);
  const guarded = (fn) => async () => {
    if (busy) return;
    busy = true;
    try {
      await fn();
    } finally {
      busy = false;
    }
  };
  const browseBtn = pushButton("Browse…", guarded(async () => {
    // Python's Windows filter is "Blender (blender.exe)"; the native dialog filters by
    // extension only (see main.js pickBlender).
    const p = await pickFile(
      "Locate Blender executable",
      [{ name: "Blender (*.exe)", extensions: ["exe"] }, { name: "All files (*)", extensions: ["*"] }],
      blenderE.value || "",
    );
    if (p) blenderE.value = p;
  }));
  const autoBtn = pushButton("Auto-detect", guarded(async () => {
    let found = null;
    try {
      found = await invoke("find_blender");
    } catch {
      found = null;
    }
    if (found) blenderE.value = found;
    else {
      await message("Auto-detect", "Could not locate Blender automatically.\nSet BLENDER_PATH or browse to the executable.", "information");
    }
  }));
  const blenderRow = el("div", "field-row");
  blenderRow.append(blenderE, browseBtn, autoBtn);
  const hint = el("div", "meta wrap",
    "Blender executable: explicit path overrides auto-detection. Leave blank to auto-detect.\n\n"
    + "Terminal alias: shell command run when an AI button is clicked.\n"
    + "  {path} = addon folder.   {cmd} = AI alias.\n"
    + `  Default:  ${defaults.terminal}\n\n`
    + "AI aliases: comma-separated. One button per alias on each row.\n"
    + "  Examples:  claude, codex, gemini\n\n"
    + "Both run via shell with cwd = addon folder.");
  const resetTerminal = pushButton("Reset terminal alias", () => { terminalE.value = defaults.terminal; });
  const terminalPage = el("div", "page form");
  terminalPage.append(
    el("label", "form-label", "Blender executable"), blenderRow,
    el("label", "form-label", "Terminal alias"), terminalE,
    el("label", "form-label", "AI aliases"), aiE,
    hint, resetTerminal,
  );
  hint.classList.add("span");
  resetTerminal.classList.add("span");

  // ---------- Fonts ----------
  const changedFont = () => run(() => doPreview());
  const uiFamily = comboBox({ items: fontCache || [], value: String(draft.ui_font_family), editable: true, onChange: changedFont });
  const monoFamily = comboBox({ items: fontCache || [], value: String(draft.mono_font_family), editable: true, onChange: changedFont });
  const sizeSpin = (v) => {
    const s = spinBox({ value: FONT_SIZE_MIN, min: FONT_SIZE_MIN, max: FONT_SIZE_MAX, onCommit: changedFont, onStep: changedFont });
    s.setValue(v);
    return s;
  };
  const uiSize = sizeSpin(draft.ui_font_size);
  const monoSize = sizeSpin(draft.mono_font_size);
  if (!fontCache) {
    invoke("list_fonts").then((f) => {
      fontCache = Array.isArray(f) ? f : [];
      uiFamily.setItems(fontCache);
      monoFamily.setItems(fontCache);
    }).catch(() => {});
  }
  const fontsPage = el("div", "page form");
  fontsPage.append(
    el("label", "form-label", "UI font family"), uiFamily,
    el("label", "form-label", "UI font size"), uiSize,
    el("label", "form-label", "Mono font family"), monoFamily,
    el("label", "form-label", "Mono font size"), monoSize,
    el("div", "meta span", "Mono font is used for the output console."),
  );

  const setFontWidgets = () => {
    uiFamily.setValue(String(draft.ui_font_family));
    uiSize.setValue(draft.ui_font_size);
    monoFamily.setValue(String(draft.mono_font_family));
    monoSize.setValue(draft.mono_font_size);
  };

  // ---------- result / preview ----------
  const fields = () => ({
    uiFamily: uiFamily.value(),
    uiSize: uiSize.value(),
    monoFamily: monoFamily.value(),
    monoSize: monoSize.value(),
    terminal: terminalE.value,
    aiAliases: aiE.value,
    blenderPath: blenderE.value,
  });
  /** `_commit_inputs`: the draft takes the font fields; returns the dialog result. */
  const commitInputs = () => {
    const r = commitSettings(draft, fields(), defaults);
    Object.assign(draft, r.theme);
    return r;
  };
  const doPreview = () => preview(commitSettings(draft, fields(), defaults).theme);

  // ---------- Colors ----------
  const colors = colorsTab({
    draft, presets, colorKeys, defaults: defaults.theme, run,
    changed: doPreview,
    onExport: guarded(async () => {
      colors.commitFocused();
      await run(() => commitInputs());
      const path = await pickSaveFile("Export theme", [{ name: "JSON theme (*.json)", extensions: ["json"] }], "blinker_theme.json");
      if (!path) return;
      try {
        await invoke("export_theme", { path, theme: { ...draft } });
      } catch (e) {
        await message("Export failed", errText(e), "critical");
      }
    }),
    onImport: guarded(async () => {
      const path = await pickFile(
        "Import theme",
        [{ name: "JSON theme (*.json)", extensions: ["json"] }, { name: "All files (*)", extensions: ["*"] }],
        "",
      );
      if (!path) return;
      let res;
      try {
        res = await invoke("import_theme", { path });
      } catch (e) {
        await message("Import failed", errText(e), "critical");
        return;
      }
      await run(async () => {
        Object.assign(draft, res.theme);
        setFontWidgets();
        colors.refreshAll();
        await colors.detect();
        doPreview();
      });
      const notes = importNotes(res.unknown, res.invalid);
      if (notes) await message("Import notes", notes, "warning");
    }),
  });

  // ---------- tab widget ----------
  const pages = [["Terminal", terminalPage], ["Fonts", fontsPage], ["Colors", colors.el]];
  const tabbar = el("div", "tabbar");
  tabbar.tabIndex = 0;
  tabbar.setAttribute("role", "tablist");
  const pane = el("div", "tabpane");
  let currentTab = 0;
  const tabs = pages.map(([label, page], i) => {
    const t = el("div", "tab", label);
    t.setAttribute("role", "tab");
    t.addEventListener("mousedown", (e) => {
      if (e.button !== 0) return;
      e.preventDefault();
      setTab(i);
      tabbar.focus();
    });
    tabbar.append(t);
    pane.append(page);
    return t;
  });
  function setTab(i) {
    currentTab = (i + pages.length) % pages.length;
    tabs.forEach((t, j) => {
      t.classList.toggle("selected", j === currentTab);
      t.setAttribute("aria-selected", String(j === currentTab));
    });
    pages.forEach(([, p], j) => p.classList.toggle("hidden", j !== currentTab));
  }
  tabbar.addEventListener("keydown", (e) => {
    if (e.key === "ArrowLeft") { e.preventDefault(); setTab(Math.max(0, currentTab - 1)); }
    else if (e.key === "ArrowRight") { e.preventDefault(); setTab(Math.min(pages.length - 1, currentTab + 1)); }
  });
  const tabWidget = el("div", "tabwidget");
  tabWidget.append(tabbar, pane);

  // ---------- buttons (QDialogButtonBox: Restore Defaults | OK Cancel Apply) ----------
  let dlg;
  const resetAll = () => run(async () => {
    for (const k of Object.keys(draft)) delete draft[k];
    Object.assign(draft, defaults.theme);
    setFontWidgets();
    colors.refreshAll();
    terminalE.value = defaults.terminal;
    aiE.value = defaults.ai_aliases;
    blenderE.value = "";
    await colors.detect();
    doPreview();
  });
  const apply = () => {
    if (busy) return;
    colors.commitFocused();
    run(() => onApply(commitInputs()));
  };
  const ok = () => {
    if (busy) return;
    colors.commitFocused();
    run(() => {
      const r = commitInputs();
      closeAll();
      dlg.close(r);
    });
  };
  const cancel = () => {
    if (busy) return;
    closeAll();
    dlg.close(null);
  };
  const closeAll = () => {
    uiFamily.closePopup();
    monoFamily.closePopup();
    colors.closePopups();
    window.removeEventListener("keydown", ctrlTab, true);
  };
  const buttons = el("div", "dlg-buttons");
  buttons.append(
    pushButton("Restore Defaults", () => { if (!busy) resetAll(); }, { cls: "left" }),
    pushButton("OK", ok),
    pushButton("Cancel", cancel),
    pushButton("Apply", apply),
  );

  // QTabWidget: Ctrl+Tab / Ctrl+Shift+Tab switch tabs from anywhere in the dialog.
  function ctrlTab(e) {
    if (e.key !== "Tab" || !e.ctrlKey || !document.querySelector(".settings")) return;
    const overlays = document.querySelectorAll(".modal-overlay");
    if (!overlays.length || !overlays[overlays.length - 1].querySelector(".settings")) return;
    e.preventDefault();
    e.stopImmediatePropagation();
    setTab(currentTab + (e.shiftKey ? -1 : 1));
  }
  window.addEventListener("keydown", ctrlTab, true);

  setTab(tab);
  dlg = openDialog({
    title: "Settings",
    cls: "settings",
    width: 760,
    height: 720,
    content: [tabWidget, buttons],
    onAccept: ok,
    onReject: cancel,
    focus: tabbar,
  });
  run(() => colors.detect());
  return dlg.result;
}
