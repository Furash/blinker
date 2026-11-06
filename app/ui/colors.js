// Colors tab of the Settings dialog (SettingsDialog._colors_tab and the colour helpers,
// blinker_ui.py:1976-2115). Edits the caller's `draft` theme in place. Hex
// normalisation and preset detection are the backend's (`normalize_hex`,
// `detect_preset` commands), so there is one implementation of each.

import { invoke } from "./api.js";
import { pushButton } from "./dialog.js";
import { comboBox } from "./combo.js";

const el = (tag, cls, text) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
};

export const CUSTOM_PRESET = "Custom";

/**
 *   draft        the dialog's draft theme (mutated)
 *   presets      [{ name, theme }] builtin presets in order
 *   colorKeys    [[key, label], ...] (COLOR_KEYS)
 *   defaults     DEFAULT_THEME
 *   run(fn)      runs fn after every earlier dialog action (serialised)
 *   onExport/onImport  the Export theme… / Import theme… buttons
 *   preset       the initial preset name (`_detect_preset()` at construction)
 * Returns { el, refresh(key), refreshAll(), detect(), commitFocused(), closePopups() }.
 */
export function colorsTab({ draft, presets, colorKeys, defaults, run, preset, onExport, onImport }) {
  const page = el("div", "page colors-page");
  const swatches = new Map();
  const hexEdits = new Map();
  const pickers = new Map();

  // ----- preset row -----
  const presetRow = el("div", "preset-row");
  const names = presets.map((p) => p.name);
  const presetCb = comboBox({
    items: [...names, CUSTOM_PRESET],
    value: preset,
    onChange: (name) => run(() => onPresetChange(name)),
  });
  presetRow.append(
    el("span", "preset-label", "Preset:"),
    presetCb,
    pushButton("Export theme…", () => onExport()),
    pushButton("Import theme…", () => onImport()),
  );

  // ----- colour grid in a scroll area -----
  const scroll = el("div", "color-scroll");
  const grid = el("div", "color-grid");
  scroll.append(grid);
  for (const [key, label] of colorKeys) {
    const sw = el("div", "swatch");
    swatches.set(key, sw);

    const hex = el("input", "qline hex");
    hex.type = "text";
    hex.maxLength = 9;
    hex.spellcheck = false;
    hex.autocomplete = "off";
    hex.dataset.key = key;
    hexEdits.set(key, hex);
    // editingFinished: Enter or focus loss
    hex.addEventListener("keydown", (e) => { if (e.key === "Enter") commitHex(key); });
    hex.addEventListener("blur", () => commitHex(key));

    // QColorDialog.getColor -> the native colour chooser of <input type="color">.
    const picker = el("input", "color-input");
    picker.type = "color";
    picker.tabIndex = -1;
    picker.addEventListener("change", () => {
      const c = picker.value.toLowerCase();
      run(async () => {
        draft[key] = c; // QColor.name(): #rrggbb, lower case
        refresh(key);
        await detect();
      });
    });
    pickers.set(key, picker);
    const pick = () => {
      picker.value = /^#[0-9a-f]{6}$/i.test(String(draft[key])) ? draft[key] : "#000000";
      try {
        picker.showPicker();
      } catch {
        picker.click();
      }
    };
    sw.addEventListener("click", pick);

    const pickBtn = pushButton("Pick…", pick);
    const resetBtn = pushButton("Reset", () => run(async () => {
      draft[key] = defaults[key];
      refresh(key);
      await detect();
    }));
    const lbl = el("span", "color-label", label);
    const swCell = el("div", "swatch-cell");
    swCell.append(sw, picker);
    grid.append(lbl, swCell, hex, pickBtn, resetBtn);
  }
  page.append(presetRow, scroll);

  function setSwatch(sw, color) {
    sw.style.backgroundColor = color;
    // Qt paints the 1 px rgba(127,127,127,0.5) border over half background, half window.
    sw.style.borderColor = `color-mix(in srgb, rgb(127 127 127) 50%, color-mix(in srgb, ${color} 50%, var(--window_bg)))`;
  }

  /** `_refresh_swatch`. */
  function refresh(key) {
    const color = String(draft[key]);
    setSwatch(swatches.get(key), color);
    hexEdits.get(key).value = color;
  }
  function refreshAll() {
    for (const [key] of colorKeys) refresh(key);
  }

  /** `_set_preset_silent(_detect_preset())`. */
  async function detect() {
    const name = await invoke("detect_preset", { theme: draft });
    presetCb.setValue(name);
  }

  /** `_on_preset_change`. */
  function onPresetChange(name) {
    const p = presets.find((x) => x.name === name);
    if (name === CUSTOM_PRESET || !p) return;
    for (const [key] of colorKeys) {
      if (key in p.theme) draft[key] = p.theme[key];
      refresh(key);
    }
  }

  /** `_commit_hex`: normalise or restore the text. */
  function commitHex(key) {
    return run(async () => {
      const hex = hexEdits.get(key);
      const norm = await invoke("normalize_hex", { s: hex.value });
      if (norm === null) {
        hex.value = String(draft[key]);
        return;
      }
      draft[key] = norm;
      refresh(key);
      await detect();
    });
  }

  /** editingFinished for a hex field that has focus (Enter on the dialog, Export). */
  function commitFocused() {
    const a = document.activeElement;
    if (a instanceof HTMLInputElement && a.dataset.key && hexEdits.get(a.dataset.key) === a) {
      return commitHex(a.dataset.key);
    }
    return Promise.resolve();
  }

  refreshAll();
  return { el: page, refresh, refreshAll, detect, commitFocused, closePopups: () => presetCb.closePopup() };
}
