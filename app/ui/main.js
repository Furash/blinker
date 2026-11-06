// Main window controller (MainWindow, blinker_ui.py:1033-1673).

import { invoke, listen, showWindow, pickFile, isDev, isMock, mockScenario } from "./api.js";
import { applyTheme, mergeTheme } from "./theme.js";
import { renderRows, updateRow, markSelected } from "./rows.js";
import { OutputPane } from "./output.js";
import { message, confirm, notImplemented } from "./dialog.js";
import {
  parseAliases, sortFavouritesFirst, selectionAfterRemove, textAfterSnapshot, runningFromStatus, SaveQueue,
} from "./logic.js";

const $ = (id) => document.getElementById(id);
const listEl = $("list");
const pane = new OutputPane($("output"));

const state = {
  config: null,
  selected: null,
  // port -> running, exactly as the backend's last `blinker://status` said (probe OR a
  // live supervised Blender). The UI never infers alive-ness itself.
  running: new Map(),
  launching: new Set(), // ports with a `launch` call in flight (Launch disabled)
  icons: new Map(), // lower-case alias -> data URI | null
  // Output of the selected port: `load` buffers events while get_output is in flight;
  // `shownSeq` is the console sequence the pane holds (events at or below it are dupes).
  load: null, // { port, buffer: [{ seq, text }] }
  shownSeq: 0,
};

const errText = (e) => (e && typeof e === "object" ? e.message ?? JSON.stringify(e) : String(e));
const folders = () => state.config.folders;

// ---------- persistence ----------

// One save in flight at a time; later requests coalesce and send the config as it is
// when that save starts (blender_path included, see launch()).
const saves = new SaveQueue(
  (config) => invoke("save_config", { config }),
  () => JSON.parse(JSON.stringify(state.config)),
  (e) => message("Save", errText(e)),
);
const save = () => saves.request();

function syncPorts() {
  return invoke("set_ports", { ports: folders().map((f) => f.port) }).catch(() => {});
}

/** Ask the backend for a fresh status of every folder port (`_poll_status`). */
async function pollNow() {
  await syncPorts();
  invoke("probe_now", { ports: folders().map((f) => f.port) }).catch(() => {});
}

// ---------- running state ----------

/** `_apply_statuses` for a full status payload: update rows whose state changed. */
function applyStatus(payload) {
  const next = runningFromStatus(folders().map((f) => f.port), payload);
  folders().forEach((f, i) => {
    const r = next.get(f.port);
    if (r !== !!state.running.get(f.port)) {
      state.running.set(f.port, r);
      updateRow(listEl, state, i, handlers);
    }
  });
}

// ---------- rendering ----------

function refreshRows() {
  renderRows(listEl, state, handlers);
  renderOutput();
}

function select(idx) {
  if (state.selected === idx) return;
  state.selected = idx;
  markSelected(listEl, idx);
  renderOutput();
}

const selectedPort = () => {
  const f = state.selected === null ? null : folders()[state.selected];
  return f ? f.port : null;
};

/** `_render_output`. */
async function renderOutput() {
  const label = $("out-label");
  const f = state.selected === null ? null : folders()[state.selected];
  if (!f) {
    label.textContent = "(no folder selected)";
    state.load = null;
    pane.setText("");
    return;
  }
  label.textContent = `${f.path}   ·   port ${f.port}`;
  const port = f.port;
  const load = { port, buffer: [] };
  state.load = load;
  let snap = { text: "", seq: 0 };
  try {
    snap = await invoke("get_output", { port });
  } catch {
    /* empty console */
  }
  if (state.load !== load || selectedPort() !== port) return; // superseded
  state.load = null;
  pane.setText(snap.text);
  // events that raced the snapshot: keep only those past it
  pane.append(textAfterSnapshot(snap.seq, load.buffer));
  state.shownSeq = Math.max(snap.seq, ...load.buffer.map((e) => e.seq));
}

function appendOutput(port, text, seq) {
  if (selectedPort() !== port) return;
  if (state.load && state.load.port === port) {
    state.load.buffer.push({ seq, text });
    return;
  }
  if (seq <= state.shownSeq) return;
  state.shownSeq = seq;
  pane.append(text);
}

// ---------- folder actions ----------

async function launch(idx, blender = null) {
  const f = folders()[idx];
  if (!f || state.launching.has(f.port)) return;
  const port = f.port;
  const redraw = () => {
    const i = folders().indexOf(f);
    if (i >= 0) updateRow(listEl, state, i, handlers);
  };
  state.launching.add(port);
  redraw();
  let retry = null;
  try {
    const persisted = await invoke("launch", { folder: f, blender });
    // The backend saved a user-picked Blender path: keep the local copy in step, or
    // the next save_config would overwrite it. (Running state arrives as a status event.)
    if (persisted) state.config.blender_path = persisted;
  } catch (e) {
    state.launching.delete(port);
    redraw();
    if (e && e.kind === "blender_not_found") {
      if (!(await confirm("Blender not found", e.message))) return;
      retry = await pickFile({ title: "Locate Blender executable", defaultPath: state.config.blender_path });
    } else {
      await message("Launch", errText(e));
    }
  } finally {
    if (state.launching.delete(port)) redraw();
  }
  if (retry && folders().includes(f)) await launch(folders().indexOf(f), retry);
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

/** `_change_port`: running is unknown on the new port until the poll it triggers. */
function changePort(idx, port) {
  const f = folders()[idx];
  if (!f || f.port === port) return;
  f.port = port;
  state.running.set(port, false);
  save();
  updateRow(listEl, state, idx, handlers);
  if (idx === state.selected) renderOutput();
  pollNow();
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
  // a no-op (false) when this app supervises nothing on the port
  invoke("terminate_process", { port: f.port }).catch(() => {});
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
  state.running = runningFromStatus(folders().map((f) => f.port), st.running || {});
  state.selected = folders().length ? 0 : null;
  if (mockScenario && mockScenario.selected !== null && mockScenario.selected < folders().length) {
    state.selected = mockScenario.selected;
  }
  disableWebIsms(dev);
  await Promise.all([loadIcons(state.config.ai_aliases), document.fonts.ready]);
  refreshRows();
  setupSplitter();
  await syncPorts();

  await Promise.all([
    listen("blinker://output", ({ payload }) => appendOutput(payload.port, payload.text, payload.seq)),
    // A full snapshot (probe OR live supervised Blender). The backend republishes it
    // after a launch and after every exit, so `blinker://exited` needs no handling here
    // (its console line is appended by the backend).
    listen("blinker://status", ({ payload }) => applyStatus(payload)),
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
