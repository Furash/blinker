// Main window controller (MainWindow, blinker_ui.py:1033-1673).

import { invoke, listen, showWindow, pickFile, isDev, isMock, mockScenario } from "./api.js";
import { applyTheme, mergeTheme } from "./theme.js";
import { renderRows, updateRow, markSelected } from "./rows.js";
import { OutputPane } from "./output.js";
import { message, confirm, notImplemented } from "./dialog.js";
import { parseAliases, sortFavouritesFirst, selectionAfterRemove } from "./logic.js";

const $ = (id) => document.getElementById(id);
const listEl = $("list");
const pane = new OutputPane($("output"));

const state = {
  config: null,
  selected: null,
  running: new Map(), // port -> shown state (probe || supervised)
  probe: new Map(), // port -> last TCP probe result
  supervised: new Set(), // ports with a Blender launched by this app and not yet exited
  icons: new Map(), // lower-case alias -> data URI | null
  loadingPort: null, // get_output in flight for this port
};

const errText = (e) => (e && typeof e === "object" ? e.message ?? JSON.stringify(e) : String(e));
const folders = () => state.config.folders;

// ---------- persistence ----------

async function save() {
  try {
    await invoke("save_config", { config: state.config });
  } catch (e) {
    await message("Save", errText(e));
  }
}

function syncPorts() {
  invoke("set_ports", { ports: folders().map((f) => f.port) }).catch(() => {});
}

// ---------- running state ----------

function computeRunning(port) {
  return !!state.probe.get(port) || state.supervised.has(port);
}

/** `_apply_statuses`: update rows whose running state changed. */
function refreshRunning() {
  folders().forEach((f, i) => {
    const r = computeRunning(f.port);
    if (r !== !!state.running.get(f.port)) {
      state.running.set(f.port, r);
      updateRow(listEl, state, i, handlers);
    }
  });
}

// ---------- rendering ----------

function refreshRows() {
  for (const f of folders()) state.running.set(f.port, computeRunning(f.port));
  renderRows(listEl, state, handlers);
  renderOutput();
}

function select(idx) {
  if (state.selected === idx) return;
  state.selected = idx;
  markSelected(listEl, idx);
  renderOutput();
}

/** `_render_output`. */
async function renderOutput() {
  const label = $("out-label");
  const f = state.selected === null ? null : folders()[state.selected];
  if (!f) {
    label.textContent = "(no folder selected)";
    state.loadingPort = null;
    pane.setText("");
    return;
  }
  label.textContent = `${f.path}   ·   port ${f.port}`;
  const port = f.port;
  state.loadingPort = port;
  let text = "";
  try {
    text = await invoke("get_output", { port });
  } catch {
    text = "";
  }
  const cur = state.selected === null ? null : folders()[state.selected];
  if (state.loadingPort !== port || !cur || cur.port !== port) return; // superseded
  state.loadingPort = null;
  pane.setText(text);
}

function appendOutput(port, text) {
  const f = state.selected === null ? null : folders()[state.selected];
  // while get_output is in flight its snapshot already holds this text
  if (f && f.port === port && state.loadingPort !== port) pane.append(text);
}

// ---------- folder actions ----------

async function launch(idx, blender = null) {
  const f = folders()[idx];
  if (!f) return;
  try {
    const persisted = await invoke("launch", { folder: f, blender });
    // The backend saved a user-picked Blender path: keep the local copy in step, or
    // the next save_config would overwrite it.
    if (persisted) state.config.blender_path = persisted;
    state.supervised.add(f.port);
    refreshRunning();
  } catch (e) {
    if (e && e.kind === "blender_not_found") {
      if (!(await confirm("Blender not found", e.message))) return;
      const picked = await pickFile({ title: "Locate Blender executable", defaultPath: state.config.blender_path });
      if (picked) await launch(idx, picked);
      return;
    }
    await message("Launch", errText(e));
  }
}

async function tcpAction(cmd, idx) {
  const f = folders()[idx];
  if (!f) return;
  try {
    // The backend appends the "[cmd] response" line to the console and emits it.
    await invoke(cmd, { port: f.port });
  } catch (e) {
    await message(cmd[0].toUpperCase() + cmd.slice(1), errText(e));
  }
}

async function clearConsole(idx) {
  const f = folders()[idx];
  if (!f) return;
  await invoke("clear_output", { port: f.port }).catch(() => {});
  if (idx === state.selected) pane.clear();
}

async function openFolder(idx) {
  const f = folders()[idx];
  if (!f) return;
  try {
    await invoke("open_folder", { path: f.path });
  } catch (e) {
    await message("Open folder", errText(e));
  }
}

async function openAi(idx, ai) {
  const f = folders()[idx];
  if (!f) return;
  try {
    await invoke("open_ai", { path: f.path, ai });
  } catch (e) {
    await message("Open AI", errText(e));
  }
}

/** `_change_port`. */
function changePort(idx, port) {
  const f = folders()[idx];
  if (!f || f.port === port) return;
  f.port = port;
  save();
  syncPorts();
  state.running.set(port, computeRunning(port)); // unknown until the next poll
  updateRow(listEl, state, idx, handlers);
  if (idx === state.selected) renderOutput();
}

/** `_toggle_fav`. */
function toggleFav(idx) {
  const target = folders()[idx];
  if (!target) return;
  target.favourite = !target.favourite;
  const selectedTarget = state.selected !== null ? folders()[state.selected] ?? null : null;
  state.config.folders = sortFavouritesFirst(folders());
  if (selectedTarget) state.selected = folders().indexOf(selectedTarget);
  save();
  syncPorts();
  refreshRows();
}

/** `_remove_folder`. */
async function removeFolder(idx) {
  const f = folders()[idx];
  if (!f) return;
  if (!(await confirm("Remove", `Remove this folder from the list?\n\n${f.path}`))) return;
  const i = folders().indexOf(f);
  if (i < 0) return;
  if (state.running.get(f.port) || state.supervised.has(f.port)) {
    invoke("terminate_process", { port: f.port }).catch(() => {});
  }
  folders().splice(i, 1);
  state.selected = selectionAfterRemove(state.selected, folders().length);
  save();
  syncPorts();
  refreshRows();
}

const handlers = {
  select,
  launch: (i) => launch(i),
  kill: (i) => tcpAction("kill", i),
  reload: (i) => tcpAction("reload", i),
  restart: (i) => tcpAction("restart", i),
  clear: clearConsole,
  openFolder,
  edit: () => notImplemented("Edit folder"), // TODO(Task 10): EditFolderDialog
  remove: removeFolder,
  fav: toggleFav,
  port: changePort,
  ai: openAi,
};

// ---------- splitter (QSplitter, vertical, 1:2 stretch, sizes 320/480) ----------

let shown = false; // before the window is shown, the splitter keeps the 320:480 proportion

function setupSplitter() {
  const split = $("split");
  const top = $("list-scroll");
  const handle = $("handle");
  const HANDLE = 4;
  const MIN_TOP = 40;
  const MIN_BOTTOM = 80;
  let avail = split.clientHeight - HANDLE;
  let topH = Math.round((avail * 320) / 800);
  const clamp = (h) => Math.max(MIN_TOP, Math.min(h, avail - MIN_BOTTOM));
  const apply = () => { top.style.height = `${Math.round(topH)}px`; };
  apply();
  new ResizeObserver(() => {
    const next = split.clientHeight - HANDLE;
    if (next === avail) return;
    if (shown) topH += (next - avail) / 3; // stretch factors 1:2
    else topH = Math.round((next * 320) / 800);
    avail = next;
    topH = clamp(topH);
    apply();
  }).observe(split);
  handle.addEventListener("pointerdown", (e) => {
    if (e.button !== 0) return;
    e.preventDefault();
    handle.setPointerCapture(e.pointerId);
    const y0 = e.clientY;
    const h0 = topH;
    const move = (ev) => { topH = clamp(h0 + ev.clientY - y0); apply(); };
    const up = () => {
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", up);
      handle.removeEventListener("pointercancel", up);
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", up);
    handle.addEventListener("pointercancel", up);
  });
}

// ---------- web-isms ----------

function disableWebIsms(dev) {
  const editable = (t) => t instanceof Element && !!t.closest("#output, input, textarea, .modal-text");
  document.addEventListener("contextmenu", (e) => {
    if (!editable(e.target)) e.preventDefault();
  });
  document.addEventListener("dragstart", (e) => e.preventDefault());
  // Browser navigation would unload the app: always blocked.
  document.addEventListener("keydown", (e) => {
    const k = e.key;
    const ctrl = e.ctrlKey || e.metaKey;
    const nav = (e.altKey && (k === "ArrowLeft" || k === "ArrowRight")) || k === "BrowserBack" || k === "BrowserForward";
    const reloadOrTools = k === "F5" || k === "F12" || k === "F3" || k === "F7"
      || (ctrl && ["r", "p", "f", "g", "s", "u", "j"].includes(k.toLowerCase()))
      || (ctrl && e.shiftKey && ["i", "c", "j"].includes(k.toLowerCase()));
    if (nav || (!dev && reloadOrTools)) e.preventDefault();
  }, true);
  document.addEventListener("mousedown", (e) => {
    if (e.button === 1 || e.button === 3 || e.button === 4) e.preventDefault(); // autoscroll, back/forward
  });
}

// ---------- startup ----------

async function loadIcons(aliases) {
  await Promise.all(parseAliases(aliases).map(async (a) => {
    const key = a.toLowerCase();
    if (state.icons.has(key)) return;
    let uri = null;
    try { uri = await invoke("alias_icon", { alias: a }); } catch { uri = null; }
    state.icons.set(key, uri || null);
    if (uri) await new Promise((res) => { const im = new Image(); im.onload = im.onerror = res; im.src = uri; });
  }));
}

async function main() {
  const [st, dev] = await Promise.all([invoke("get_state"), isDev()]);
  state.config = st.config;
  applyTheme(mergeTheme(st.default_theme, st.config.theme));
  state.config.theme = mergeTheme(st.default_theme, st.config.theme);
  $("config-label").textContent = `Config: ${st.config_path}`;
  for (const [p, r] of Object.entries(st.running || {})) state.probe.set(Number(p), !!r);
  state.selected = folders().length ? 0 : null;
  if (mockScenario && mockScenario.selected !== null && mockScenario.selected < folders().length) {
    state.selected = mockScenario.selected;
  }
  disableWebIsms(dev);
  await Promise.all([loadIcons(state.config.ai_aliases), document.fonts.ready]);
  refreshRows();
  setupSplitter();
  syncPorts();

  await Promise.all([
    listen("blinker://output", ({ payload }) => appendOutput(payload.port, payload.text)),
    listen("blinker://status", ({ payload }) => {
      for (const [p, r] of Object.entries(payload)) state.probe.set(Number(p), !!r);
      refreshRunning();
    }),
    listen("blinker://exited", ({ payload }) => {
      // The backend already appended "[blender exited code N]"; a Blender not started by
      // this app may still answer on the port, so fall back to the last probe.
      state.supervised.delete(payload.port);
      refreshRunning();
    }),
    listen("blinker://terminal-error", ({ payload }) => message("Terminal failed", payload.message)),
  ]);
  // First status poll, now that the status listener is registered (get_state does not probe).
  invoke("probe_now", { ports: folders().map((f) => f.port) }).catch(() => {});

  $("act-add").addEventListener("click", () => notImplemented("Add Folder")); // TODO(Task 10)
  $("act-settings").addEventListener("click", () => notImplemented("Settings")); // TODO(Task 11)
  $("act-update").addEventListener("click", () => notImplemented("Updates")); // TODO(Task 12)

  // Show only after the themed first frame is painted (no flash of unthemed content).
  // A hidden WebView2 may throttle rAF, so a short timeout backs it up.
  const painted = new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(() => r("raf"))));
  window.__blinker_show_trigger = await Promise.race([painted, new Promise((r) => setTimeout(() => r("timeout"), 100))]);
  await showWindow();
  window.__blinker_ready_ms = performance.now();
  invoke("report_ready", { ms: window.__blinker_ready_ms }).catch(() => {});
  // Mock mode: headless screenshot browsers resize the viewport after load; keep the
  // initial proportion so captures match the Qt reference.
  shown = !isMock;
}

main().catch(async (e) => {
  console.error(e);
  await showWindow().catch(() => {});
  message("Blinker UI", `Startup failed:\n${errText(e)}`);
});

// exposed for later tasks (Settings live preview, dialogs)
export { state, applyTheme, refreshRows, save };
