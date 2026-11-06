// In-memory stand-in for the Tauri backend, used when the page is opened in a plain
// browser (no window.__TAURI__) so the UI can be viewed and screenshotted.
// URL options: ?theme=tokyo|frigus|light  &scene=state|plain|empty
//   &dialog=scan|edit|noaddons|settings  open that dialog after startup (screenshots)
//   &tab=0|1|2                  Settings: the tab shown first (screenshots)
//   &blender=missing            launch fails with blender_not_found until a path is picked
// The folders, output, config label and scan results match tools/grab_reference.py.
// Native pickers: `window.__blinkerMockPicks` = [value|null, ...] answers the next
// picks in order (null = cancelled); otherwise canned paths. Every pick request is
// recorded in `window.__blinkerPicks`.

const TOKYO = {
  ui_font_family: "Segoe UI", ui_font_size: 10, mono_font_family: "Cascadia Mono", mono_font_size: 10,
  window_bg: "#1e1f22", panel_bg: "#2b2d30", row_bg: "#2b2d30", row_selected_bg: "#3b3e44",
  row_hover_bg: "#34373c", row_border: "#3b3e44", row_text_fg: "#e8eaed", row_meta_fg: "#9aa0a6",
  accent_fg: "#7aa2f7", status_running_fg: "#9ece6a", status_stopped_fg: "#7d8590",
  output_bg: "#16181d", output_fg: "#d4d4d4", button_bg: "#3b3e44", button_hover_bg: "#4a4e55",
  button_text_fg: "#e8eaed", input_bg: "#2b2d30", input_text_fg: "#e8eaed",
};
const PRESET_NAMES = ["Tokyo Night (default)", "Frigus Nox", "Light"];
const COLOR_KEYS = [
  ["window_bg", "Window background"], ["panel_bg", "Panel background"], ["row_bg", "Row background"],
  ["row_selected_bg", "Selected row"], ["row_hover_bg", "Hover row"], ["row_border", "Row border"],
  ["row_text_fg", "Row text"], ["row_meta_fg", "Row meta"], ["accent_fg", "Accent / selected border"],
  ["status_running_fg", "Status running"], ["status_stopped_fg", "Status stopped"],
  ["output_bg", "Output background"], ["output_fg", "Output text"], ["button_bg", "Button"],
  ["button_hover_bg", "Button hover"], ["button_text_fg", "Button text"], ["input_bg", "Input background"],
  ["input_text_fg", "Input text"],
];
const PRESETS = {
  tokyo: TOKYO,
  frigus: {
    ...TOKYO, window_bg: "#21252b", panel_bg: "#2c3037", row_bg: "#2c3037", row_selected_bg: "#3a4f6b",
    row_hover_bg: "#363b44", row_border: "#363b44", row_text_fg: "#e7e9eb", row_meta_fg: "#858585",
    accent_fg: "#6d95c0", status_running_fg: "#79c3ab", status_stopped_fg: "#587584", output_bg: "#21252b",
    output_fg: "#c6c6c6", button_bg: "#2c3037", button_hover_bg: "#363b44", button_text_fg: "#e7e9eb",
    input_bg: "#21252b", input_text_fg: "#e7e9eb",
  },
  light: {
    ...TOKYO, window_bg: "#fafbfc", panel_bg: "#eef0f3", row_bg: "#ffffff", row_selected_bg: "#e3edff",
    row_hover_bg: "#f3f5f8", row_border: "#d0d4d9", row_text_fg: "#1f2328", row_meta_fg: "#656d76",
    accent_fg: "#0969da", status_running_fg: "#1a7f37", status_stopped_fg: "#6e7681", output_bg: "#f6f8fa",
    output_fg: "#1f2328", button_bg: "#ffffff", button_hover_bg: "#eef0f3", button_text_fg: "#1f2328",
    input_bg: "#ffffff", input_text_fg: "#1f2328",
  },
};

const SAMPLE_OUTPUT =
  "$ C:/Program Files/Blender Foundation/Blender 4.5/blender.exe --python bootstrap.py\n" +
  "  addon:  D:\\git\\blender_addons\\ccp_exporter\n" +
  "  module: bl_ext.user_default.ccp_exporter\n" +
  "  port:   9877\n\n" +
  "Blender 4.5.3 LTS (hash 67807e1800cc built 2025-09-09 01:31:16)\n" +
  "[blinker] bootstrap server listening on 127.0.0.1:9877\n" +
  "[blinker] enabled bl_ext.user_default.ccp_exporter\n" +
  "[reload] ok\n";

const params = new URLSearchParams(location.search);
const scene = params.get("scene") || "state";
const theme = PRESETS[params.get("theme")] || TOKYO;

const folders = scene === "empty" ? [] : [
  { path: "D:/git/Blender_CCP_Tools", port: 9876, repo: "blinker", module: "", blend: "", favourite: true },
  { path: "D:/git/blender_addons/ccp_exporter", port: 9877, repo: "user_default", module: "ccp_exporter",
    blend: "D:/scenes/export_test.blend", favourite: false },
  { path: "D:/git/blender_addons/quick_tools", port: 9878, repo: "blinker", module: "", blend: "", favourite: false },
];

const config = {
  folders, terminal: 'wt -d "{path}" {cmd}', ai_aliases: "claude, codex", blender_path: "", theme: { ...theme },
};
// tools/grab_reference.py SCAN_ROOT / SCAN_FOUND
const SCAN_ROOT = String.raw`D:\git\blender_addons`;
const SCAN_FOUND = [
  { path: String.raw`D:\git\blender_addons\asset_tools`, kind: "extension" },
  { path: String.raw`D:\git\blender_addons\ccp_exporter`, kind: "extension" },
  { path: String.raw`D:\git\blender_addons\legacy\mesh_cleanup`, kind: "legacy" },
  { path: String.raw`D:\git\blender_addons\quick_tools_v2`, kind: "extension" },
  { path: String.raw`D:\git\blender_addons\uv_helpers`, kind: "legacy" },
];
// a folder that is itself an addon
const SINGLE_ADDON = String.raw`D:\git\single_addon`;
const norm = (p) => String(p).replace(/\//g, "\\").replace(/\\+$/, "").toLowerCase();

const output = new Map(scene === "state" ? [[9877, SAMPLE_OUTPUT]] : []);
const alive = new Set(scene === "state" ? [9876] : []);
const listeners = new Map();

/** UI scenario hints used only in mock mode (initial selection for screenshots). */
export const scenario = { selected: scene === "state" ? 1 : null, dialog: params.get("dialog"), tab: Number(params.get("tab")) || 0 };

window.__blinkerPicks = [];
/** Native picker stand-in (api.js pickFolder / pickFile). */
export async function pick(kind, opts) {
  window.__blinkerPicks.push({ kind, ...opts });
  const queue = window.__blinkerMockPicks;
  if (Array.isArray(queue) && queue.length) return queue.shift();
  if (kind === "folder") return SCAN_ROOT;
  if (kind === "save") return `C:\\Users\\user\\${opts.defaultPath || "export.json"}`;
  if ((opts.filters || []).some((f) => f.extensions.includes("json"))) return MOCK_IMPORT;
  if ((opts.filters || []).some((f) => f.extensions.includes("blend"))) return String.raw`D:\scenes\other.blend`;
  return String.raw`C:\Program Files\Blender Foundation\Blender 4.5\blender.exe`;
}

function emit(event, payload) {
  for (const cb of listeners.get(event) || []) cb({ event, payload });
}

// Per-port console sequence (bytes ever appended), as the backend stamps it.
const seqs = new Map([...output].map(([p, t]) => [p, t.length]));

function push(port, text) {
  output.set(port, (output.get(port) || "") + text);
  const seq = (seqs.get(port) || 0) + text.length;
  seqs.set(port, seq);
  emit("blinker://output", { port, text, seq });
}

const statusPayload = () => Object.fromEntries(config.folders.map((f) => [String(f.port), alive.has(f.port)]));
const publishStatus = () => emit("blinker://status", statusPayload());

const clone = (v) => JSON.parse(JSON.stringify(v));

const commands = {
  get_state: () => ({
    config: clone(config),
    config_path: "C:\\Users\\user\\AppData\\Roaming\\BlinkerUI\\blinker_ui_config.json",
    version: "0.1.0",
    running: statusPayload(),
    default_theme: { ...TOKYO },
    presets: PRESET_NAMES,
    preset_themes: [PRESETS.tokyo, PRESETS.frigus, PRESETS.light].map((t) => ({ ...t })),
    color_keys: COLOR_KEYS,
    default_terminal: 'wt -d "{path}" {cmd}',
    default_ai_aliases: "claude, codex",
  }),
  save_config: ({ config: c }) => { Object.assign(config, clone(c)); },
  launch: ({ folder, blender }) => {
    if (!/^D:[\\/]/.test(folder.path)) {
      throw { kind: "path_not_found", message: `Path not found:\n${folder.path}` };
    }
    if (alive.has(folder.port)) {
      throw { kind: "spawn_failed", message: `Port ${folder.port} is already used by a running Blender.` };
    }
    let persisted = null;
    if (params.get("blender") === "missing" && !config.blender_path) {
      if (!blender) {
        throw { kind: "blender_not_found", message: "Could not locate Blender automatically.\n\nPick the Blender executable now?" };
      }
      config.blender_path = persisted = blender;
    }
    alive.add(folder.port);
    push(folder.port, `$ blender.exe --python bootstrap.py\n  port:   ${folder.port}\n\n`);
    publishStatus();
    return persisted;
  },
  reload: ({ port }) => { const l = "[reload] ok\n"; push(port, l); return l; },
  restart: ({ port }) => { const l = "[restart] ok\n"; push(port, l); return l; },
  kill: ({ port }) => {
    const l = "[kill] process killed\n";
    push(port, l);
    alive.delete(port);
    setTimeout(() => { push(port, "\n[blender exited code 1]\n"); emit("blinker://exited", { port, code: 1 }); publishStatus(); }, 50);
    return l;
  },
  terminate_process: ({ port }) => alive.delete(port),
  clear_output: ({ port }) => { output.delete(port); return seqs.get(port) || 0; },
  status_now: () => { setTimeout(publishStatus, 0); },
  get_output: ({ port }) => ({ text: output.get(port) || "", seq: seqs.get(port) || 0 }),
  open_ai: () => {},
  open_folder: () => {},
  set_ports: () => {},
  alias_icon: ({ alias }) => (["claude", "codex"].includes(alias.toLowerCase())
    ? `/icons/${alias.toLowerCase()}.svg` : null),
  // &dialog=noaddons: the scan root holds nothing (the "No addons" box)
  scan_addons: ({ root }) => (norm(root) === norm(SCAN_ROOT) && scenario.dialog !== "noaddons" ? SCAN_FOUND : []),
  addon_kind_of: ({ path }) => (norm(path) === norm(SINGLE_ADDON) ? "legacy"
    : SCAN_FOUND.find((a) => norm(a.path) === norm(path))?.kind ?? null),
  find_blender: () => null,
  is_dev: () => true,
  report_ready: () => {},
  probe_now: () => { setTimeout(publishStatus, 0); },
  // Settings (the real ones are theme.rs; these stand-ins only serve the mock UI)
  normalize_hex: ({ s }) => mockNormalizeHex(s),
  detect_preset: ({ theme: t }) => {
    const i = [PRESETS.tokyo, PRESETS.frigus, PRESETS.light]
      .findIndex((p) => COLOR_KEYS.every(([k]) => t[k] === p[k]));
    return i < 0 ? "Custom" : PRESET_NAMES[i];
  },
  export_theme: ({ path, theme: t }) => { files.set(path, JSON.stringify(t, null, 2)); },
  import_theme: ({ path }) => {
    if (!files.has(path)) throw `No such file: ${path}`;
    let data;
    try { data = JSON.parse(files.get(path)); } catch (e) { throw String(e.message); }
    if (!data || typeof data !== "object" || Array.isArray(data)) throw "Theme file must be a JSON object";
    const out = { theme: {}, unknown: [], invalid: [] };
    for (const [k, v] of Object.entries(data)) {
      if (!(k in TOKYO)) { out.unknown.push(k); continue; }
      if (k.endsWith("_font_size")) {
        const n = typeof v === "number" ? Math.trunc(v) : /^\s*[+-]?\d+\s*$/.test(String(v)) ? parseInt(v, 10) : NaN;
        if (Number.isNaN(n) || typeof v === "object") out.invalid.push(k); else out.theme[k] = n;
      } else if (k.endsWith("_font_family")) out.theme[k] = String(v);
      else {
        const h = mockNormalizeHex(String(v));
        if (h === null) out.invalid.push(k); else out.theme[k] = h;
      }
    }
    return out;
  },
  // Check for updates (texts as updater.rs). `&update=available` offers v0.2.0, whose
  // download then fails the SHA256 check (the mock never quits).
  check_for_updates: () => (params.get("update") === "available"
    ? {
      kind: "available",
      offer: {
        current: "0.1.0", tag: "v0.2.0", notes_preview: "\n\nMock release notes.",
        zip: { name: "BlinkerUI-v0.2.0.zip", url: "https://example.invalid/BlinkerUI-v0.2.0.zip" },
        sha: { name: "BlinkerUI-v0.2.0.zip.sha256", url: "https://example.invalid/BlinkerUI-v0.2.0.zip.sha256" },
      },
      dialog: { title: "Update available", icon: "question", text: "A new version is available.\n\nCurrent:  v0.1.0\n"
        + "Latest:   v0.2.0\n\nBlinkerUI will download, close, replace itself, and relaunch.\n\nMock release notes." },
    }
    : { kind: "up_to_date", dialog: { title: "Updates", icon: "information", text: "You are on the latest version (v0.1.0)." } }),
  apply_update: () => ({
    kind: "sha_mismatch", expected: "aa", actual: "bb",
    dialog: { title: "Updates", icon: "critical", text: "SHA256 mismatch — aborting.\n\nexpected aa\nactual   bb" },
  }),
  continue_update: () => ({ kind: "started" }),
  cancel_update: () => {},
  list_fonts: () => ["Arial", "Cascadia Code", "Cascadia Mono", "Consolas", "Courier New", "Segoe UI",
    "Segoe UI Semibold", "Tahoma", "Times New Roman", "Verdana"],
};

function mockNormalizeHex(s) {
  let h = String(s).trim().replace(/^#+/, "");
  if (/^[0-9a-f]{3}$/i.test(h)) h = h.replace(/./g, "$&$&");
  return /^[0-9a-f]{6}$/i.test(h) ? `#${h.toLowerCase()}` : null;
}

// Theme files written by export_theme (mock). The canned import file is Frigus Nox with
// an unknown key and an invalid value (shows the Import notes box).
const MOCK_IMPORT = String.raw`C:\Users\user\frigus_custom.json`;
const files = new Map([[MOCK_IMPORT,
  JSON.stringify({ ...PRESETS.frigus, window_bg: "nope", launchers: [] })]]);

export async function invoke(cmd, args = {}) {
  const f = commands[cmd];
  if (!f) throw `mock: unknown command ${cmd}`;
  return clone(f(args) ?? null);
}

export async function listen(event, cb) {
  if (!listeners.has(event)) listeners.set(event, new Set());
  listeners.get(event).add(cb);
  return () => listeners.get(event).delete(cb);
}

setInterval(publishStatus, 2000);

// Screenshot tooling: with &dialog=..., dialogs open at the top-left corner, so the
// client area is at (1, 31) (frame border, title bar).
if (scenario.dialog) document.documentElement.classList.add("dlgshot");
