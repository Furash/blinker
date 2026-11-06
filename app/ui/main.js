// Main window controller (MainWindow, blinker_ui.py:1033-1673).

import { invoke, listen, showWindow, pickFile, pickFolder, isDev, isMock, mockScenario } from "./api.js";
import { applyTheme, mergeTheme } from "./theme.js";
import { renderRows, updateRow, markSelected } from "./rows.js";
import { OutputPane } from "./output.js";
import { message, confirm } from "./dialog.js";
import { scanDialog } from "./scan.js";
import { editDialog } from "./edit.js";
import { settingsDialog } from "./settings.js";
import {
  parseAliases, sortFavouritesFirst, selectionAfterRemove, runningFromStatus, SaveQueue, ConsoleFeed,
  appendFolders,
} from "./logic.js";

// Startup steps (epoch ms) for the BLINKER_READY_LOG timeline (report_ready).
const marks = { js_start: performance.timeOrigin + performance.now() };
const mark = (k) => { marks[k] = performance.timeOrigin + performance.now(); };

const $ = (id) => document.getElementById(id);
const listEl = $("list");
const pane = new OutputPane($("output"));

const state = {
  config: null,
  selected: null,
  // port -> running, exactly as the backend's last `blinker://status` said (probe OR a
  // live supervised Blender). The UI never infers alive-ness itself.
  running: new Map(),
  // port -> "pending" (launch call in flight) | "replied" (waiting for the next status).
  // Launch stays disabled until a status built after the launch has been applied.
  launching: new Map(),
  icons: new Map(), // lower-case alias -> data URI | null
  // Console of the selected port: snapshot/clear threshold and the in-flight buffer.
  feed: new ConsoleFeed(),
  feedToken: null, // identifies the snapshot/clear in flight (superseded ones are ignored)
};

const errText = (e) => (e && typeof e === "object" ? e.message ?? JSON.stringify(e) : String(e));
const folders = () => state.config.folders;

// ---------- persistence ----------

// One save in flight at a time; later requests coalesce and send the config as it is
// when that save starts (blender_path included, see launch()).
const saves = new SaveQueue(
  (config) => invoke("save_config", { config }),
  () => JSON.parse(JSON.stringify(state.config)),
  (e) => message("Save", errText(e), "critical"),
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
  // Ports whose state changed; every row on such a port is redrawn (two folders can
  // share a port: Python lets the Edit dialog and the port box set any port).
  const dirty = new Set();
  for (const [port, r] of next) {
    if (state.launching.get(port) === "replied") {
      state.launching.delete(port);
      dirty.add(port);
    }
    if (r !== !!state.running.get(port)) {
      state.running.set(port, r);
      dirty.add(port);
    }
  }
  folders().forEach((f, i) => {
    if (dirty.has(f.port)) updateRow(listEl, state, i, handlers);
  });
}

/** After a folder moved to `port`: unknown until the next poll, unless another folder
 *  already shows that port's state. */
function portMoved(f) {
  if (!folders().some((o) => o !== f && o.port === f.port)) state.running.set(f.port, false);
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
  const token = {};
  state.feedToken = token;
  if (!f) {
    label.textContent = "(no folder selected)";
    pane.setText("");
    return;
  }
  label.textContent = `${f.path}   ·   port ${f.port}`;
  const port = f.port;
  state.feed.begin();
  let snap = { text: "", seq: 0 };
  try {
    snap = await invoke("get_output", { port });
  } catch {
    /* empty console */
  }
  if (state.feedToken !== token || selectedPort() !== port) return; // superseded
  pane.setText(snap.text);
  pane.append(state.feed.end(snap.seq)); // events that raced the snapshot, past it only
}

function appendOutput(port, text, seq) {
  if (selectedPort() !== port) return;
  pane.append(state.feed.event(seq, text));
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
  state.launching.set(port, "pending");
  redraw();
  let retry = null;
  try {
    const persisted = await invoke("launch", { folder: f, blender });
    // The backend saved a user-picked Blender path: keep the local copy in step and
    // queue a save, so neither a later nor an older in-flight save loses it.
    if (persisted) {
      state.config.blender_path = persisted;
      save();
    }
    // Running state comes from status events. Keep Launch disabled until a status built
    // after this launch is applied: ask for one now (no probe).
    state.launching.set(port, "replied");
    invoke("status_now").catch(() => {});
  } catch (e) {
    state.launching.delete(port);
    redraw();
    if (e && e.kind === "blender_not_found") {
      if (!(await confirm("Blender not found", e.message))) return;
      retry = await pickBlender();
    } else {
      await message("Launch", errText(e), "critical");
    }
  }
  if (retry && folders().includes(f)) await launch(folders().indexOf(f), retry);
}

/** `_prompt_blender_path`: the picked file, or null. */
function pickBlender() {
  // Python's Windows filter is "Blender (blender.exe)"; the native dialog filters by
  // extension only, so it lists every .exe.
  return pickFile(
    "Locate Blender executable",
    [{ name: "Blender (*.exe)", extensions: ["exe"] }, { name: "All files", extensions: ["*"] }],
    state.config.blender_path || "",
  );
}

async function tcpAction(cmd, idx) {
  const f = folders()[idx];
  if (!f) return;
  try {
    // The backend appends the "[cmd] response" line to the console and emits it.
    await invoke(cmd, { port: f.port });
  } catch (e) {
    await message(cmd[0].toUpperCase() + cmd.slice(1), errText(e), "critical");
  }
}

async function clearConsole(idx) {
  const f = folders()[idx];
  if (!f) return;
  const port = f.port;
  const shown = selectedPort() === port;
  const token = {};
  if (shown) {
    // Like a snapshot of an empty console: events in flight are buffered and only
    // those newer than the clear are shown.
    state.feedToken = token;
    state.feed.begin();
    pane.clear();
  }
  let seq = null;
  try {
    seq = await invoke("clear_output", { port });
  } catch {
    seq = null;
  }
  if (!shown || state.feedToken !== token || selectedPort() !== port) return;
  if (seq === null) {
    renderOutput(); // clear failed: show what the backend has
    return;
  }
  pane.clear();
  pane.append(state.feed.end(seq));
}

async function openFolder(idx) {
  const f = folders()[idx];
  if (!f) return;
  try {
    await invoke("open_folder", { path: f.path });
  } catch (e) {
    await message("Open folder", errText(e), "critical");
  }
}

async function openAi(idx, ai) {
  const f = folders()[idx];
  if (!f) return;
  try {
    await invoke("open_ai", { path: f.path, ai });
  } catch (e) {
    await message("Open AI", errText(e), "warning");
  }
}

/** `_change_port`: running is unknown on the new port until the poll it triggers. */
function changePort(idx, port) {
  const f = folders()[idx];
  if (!f || f.port === port) return;
  f.port = port;
  portMoved(f);
  save();
  updateRow(listEl, state, idx, handlers);
  if (idx === state.selected) renderOutput();
  pollNow();
}

/** `_add_folder`: pick a folder; add it if it is an addon, else scan it. */
async function addFolder() {
  const root = await pickFolder("Select addon folder or parent folder to scan");
  if (!root) return;
  let toAdd;
  try {
    const kind = await invoke("addon_kind_of", { path: root });
    if (kind) {
      toAdd = [root];
    } else {
      const found = await invoke("scan_addons", { root });
      if (!found.length) {
        await message("No addons", `No Blender addons or extensions found under:\n${root}`, "information");
        return;
      }
      const picked = await scanDialog(root, found, folders());
      if (!picked) return;
      toAdd = picked.map((a) => a.path);
    }
  } catch (e) {
    await message("Add Folder", errText(e), "critical");
    return;
  }
  const sel = appendFolders(folders(), toAdd);
  if (sel === null) return;
  state.selected = sel;
  save();
  syncPorts();
  refreshRows();
}

/** `_edit_folder`: OK writes the fields back, then save and re-render. */
async function editFolder(idx) {
  const f = folders()[idx];
  if (!f) return;
  const res = await editDialog(f, { portLocked: !!state.running.get(f.port) });
  if (!res || !folders().includes(f)) return;
  const portChanged = res.port !== f.port;
  Object.assign(f, res);
  if (portChanged) portMoved(f);
  save();
  syncPorts();
  refreshRows();
  if (portChanged) pollNow();
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

// ---------- settings ----------

/** Shared by OK and Apply (`_open_settings` / `SettingsDialog._apply`). */
async function commitSettings(r) {
  state.config.theme = { ...r.theme };
  state.config.terminal = r.terminal;
  state.config.ai_aliases = r.ai_aliases;
  state.config.blender_path = r.blender_path;
  save();
  applyTheme(state.config.theme);
  await loadIcons(state.config.ai_aliases); // AI buttons follow ai_aliases
  refreshRows();
}

/** `_open_settings`: OK commits like Apply; Cancel changes nothing (an Apply stays). */
async function openSettings() {
  const c = state.config;
  const res = await settingsDialog(
    { theme: c.theme, terminal: c.terminal, ai_aliases: c.ai_aliases, blender_path: c.blender_path },
    state.settingsInfo,
    { onApply: commitSettings, tab: mockScenario ? mockScenario.tab : 0 },
  );
  if (res) await commitSettings(res);
}

// ---------- Check for updates (`_check_for_updates`; texts and branches: updater.rs) ----------

let updating = false;

const showBox = (d) => message(d.title, d.text, d.icon);

/**
 * Check, ask, download + verify, start the updater (the backend then quits the app).
 * Python blocks the window during the request and the download; here a second click is
 * ignored meanwhile and the cursor shows progress.
 */
async function checkForUpdates() {
  if (updating) return;
  updating = true;
  const busy = (on) => document.body.classList.toggle("busy", on);
  try {
    busy(true);
    const r = await invoke("check_for_updates").catch((e) => ({
      dialog: { title: "Updates", text: `Could not check for updates:\n${e}`, icon: "warning" },
    }));
    busy(false);
    if (r.kind !== "available") return void (await showBox(r.dialog));
    if (!(await confirm(r.dialog.title, r.dialog.text))) return;

    // The backend applies the offer it kept from this check (no URLs from the webview).
    busy(true);
    let a = await invoke("apply_update").catch((e) => ({ kind: "download_failed", dialog: { title: "Updates", text: `Download failed:\n${e}`, icon: "critical" } }));
    busy(false);
    if (a.kind === "sha_unverifiable") {
      if (!(await confirm(a.dialog.title, a.dialog.text, "warning"))) {
        await invoke("cancel_update", { zipPath: a.zip_path }).catch(() => {});
        return;
      }
      a = await invoke("continue_update", { zipPath: a.zip_path }).catch((e) => ({
        kind: "launch_failed", dialog: { title: "Updates", text: `Could not launch updater:\n${e}`, icon: "critical" },
      }));
    }
    // "started": the backend has spawned the updater and is quitting the app.
    if (a.kind !== "started") await showBox(a.dialog);
  } finally {
    busy(false);
    updating = false;
  }
}

const handlers = {
  select,
  launch: (i) => launch(i),
  kill: (i) => tcpAction("kill", i),
  reload: (i) => tcpAction("reload", i),
  restart: (i) => tcpAction("restart", i),
  clear: clearConsole,
  openFolder,
  edit: editFolder,
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
  mark("get_state");
  state.config = st.config;
  applyTheme(mergeTheme(st.default_theme, st.config.theme));
  state.config.theme = mergeTheme(st.default_theme, st.config.theme);
  state.settingsInfo = {
    presets: st.presets.map((name, i) => ({ name, theme: st.preset_themes[i] })),
    colorKeys: st.color_keys,
    defaults: { theme: st.default_theme, terminal: st.default_terminal, ai_aliases: st.default_ai_aliases },
  };
  $("config-label").textContent = `Config: ${st.config_path}`;
  state.running = runningFromStatus(folders().map((f) => f.port), st.running || {});
  state.selected = folders().length ? 0 : null;
  if (mockScenario && mockScenario.selected !== null && mockScenario.selected < folders().length) {
    state.selected = mockScenario.selected;
  }
  disableWebIsms(dev);
  await Promise.all([loadIcons(state.config.ai_aliases), document.fonts.ready]);
  mark("icons_fonts");
  refreshRows();
  setupSplitter();
  mark("rendered");
  await syncPorts();

  await Promise.all([
    listen("blinker://output", ({ payload }) => appendOutput(payload.port, payload.text, payload.seq)),
    // A full snapshot (probe OR live supervised Blender). The backend republishes it
    // after a launch and after every exit, so `blinker://exited` needs no handling here
    // (its console line is appended by the backend).
    listen("blinker://status", ({ payload }) => applyStatus(payload)),
    listen("blinker://terminal-error", ({ payload }) => message("Terminal failed", payload.message, "critical")),
  ]);
  // First status poll, now that the status listener is registered (get_state does not probe).
  invoke("probe_now", { ports: folders().map((f) => f.port) }).catch(() => {});

  $("act-add").addEventListener("click", addFolder);
  $("act-settings").addEventListener("click", openSettings);
  $("act-update").addEventListener("click", checkForUpdates);

  // Show only after the themed first frame is painted (no flash of unthemed content).
  // A hidden WebView2 may throttle rAF, so a short timeout backs it up.
  const painted = new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(() => r("raf"))));
  window.__blinker_show_trigger = await Promise.race([painted, new Promise((r) => setTimeout(() => r("timeout"), 100))]);
  mark(`show_${window.__blinker_show_trigger}`);
  await showWindow();
  mark("shown");
  window.__blinker_ready_ms = performance.now();
  reportReady(window.__blinker_ready_ms);
  // Mock mode: headless screenshot browsers resize the viewport after load; keep the
  // initial proportion so captures match the Qt reference.
  shown = !isMock;
  // Mock screenshots: open a dialog (canned picks, see mock.js).
  const d = mockScenario && mockScenario.dialog;
  if (d === "scan" || d === "noaddons") addFolder();
  else if (d === "edit") editFolder(1);
  else if (d === "settings") openSettings();
}

/** The window is up: a second launch may now show and raise it (lib.rs `bring_to_front`). */
function reportReady(ms) {
  const nav = performance.getEntriesByType("navigation")[0];
  if (nav) marks.dom_content_loaded = performance.timeOrigin + nav.domContentLoadedEventStart;
  marks.nav_start = performance.timeOrigin;
  invoke("report_ready", { ms, marks }).catch(() => {});
}

main().catch(async (e) => {
  console.error(e);
  await showWindow().catch(() => {});
  message("Blinker UI", `Startup failed:\n${errText(e)}`, "critical");
});

// exposed for later tasks (dialogs)
export { state, applyTheme, refreshRows, save };
