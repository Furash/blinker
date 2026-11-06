// In-memory stand-in for the Tauri backend, used when the page is opened in a plain
// browser (no window.__TAURI__) so the UI can be viewed and screenshotted.
// URL options: ?theme=tokyo|frigus|light  &scene=state|plain|empty
// The folders, output and config label match tools/grab_reference.py.

const TOKYO = {
  ui_font_family: "Segoe UI", ui_font_size: 10, mono_font_family: "Cascadia Mono", mono_font_size: 10,
  window_bg: "#1e1f22", panel_bg: "#2b2d30", row_bg: "#2b2d30", row_selected_bg: "#3b3e44",
  row_hover_bg: "#34373c", row_border: "#3b3e44", row_text_fg: "#e8eaed", row_meta_fg: "#9aa0a6",
  accent_fg: "#7aa2f7", status_running_fg: "#9ece6a", status_stopped_fg: "#7d8590",
  output_bg: "#16181d", output_fg: "#d4d4d4", button_bg: "#3b3e44", button_hover_bg: "#4a4e55",
  button_text_fg: "#e8eaed", input_bg: "#2b2d30", input_text_fg: "#e8eaed",
};
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
const output = new Map(scene === "state" ? [[9877, SAMPLE_OUTPUT]] : []);
const alive = new Set(scene === "state" ? [9876] : []);
const listeners = new Map();

/** UI scenario hints used only in mock mode (initial selection for screenshots). */
export const scenario = { selected: scene === "state" ? 1 : null };

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
    presets: ["Tokyo Night (default)", "Frigus Nox", "Light"],
    color_keys: [],
  }),
  save_config: ({ config: c }) => { Object.assign(config, clone(c)); },
  launch: ({ folder }) => {
    if (!folder.path.startsWith("D:/")) {
      throw { kind: "path_not_found", message: `Path not found:\n${folder.path}` };
    }
    alive.add(folder.port);
    push(folder.port, `$ blender.exe --python bootstrap.py\n  port:   ${folder.port}\n\n`);
    publishStatus();
    return null;
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
  scan_addons: () => [],
  addon_kind_of: () => null,
  find_blender: () => null,
  is_dev: () => true,
  report_ready: () => {},
  probe_now: () => { setTimeout(publishStatus, 0); },
};

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
