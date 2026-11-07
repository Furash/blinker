// Pure helpers (no DOM, no Tauri) so they can be checked in selftest.html.

export const OUTPUT_MAX_LINES = 20000; // QPlainTextEdit.setMaximumBlockCount(20000)
export const PORT_MIN = 1024;
export const PORT_MAX = 65535;
export const PORT_BASE = 9876;

/** FolderRow._meta_text (blinker_ui.py:930-939). */
export function metaText(f) {
  const parts = [];
  if (f.module) parts.push(`module=${f.module}`);
  if (f.repo && f.repo !== "blinker") parts.push(`repo=${f.repo}`);
  if (f.blend) parts.push(`blend=${baseName(f.blend)}`);
  return parts.join("   ·   ");
}

/** pathlib.Path(p).name for Windows and POSIX separators. */
export function baseName(p) {
  const s = String(p).replace(/[\\/]+$/, "");
  const i = Math.max(s.lastIndexOf("/"), s.lastIndexOf("\\"));
  return i >= 0 ? s.slice(i + 1) : s;
}

/** Stable sort, favourites first (`_toggle_fav` / `_sort_folders`). Returns a new array. */
export function sortFavouritesFirst(folders) {
  return folders
    .map((f, i) => [f, i])
    .sort((a, b) => (!a[0].favourite - !b[0].favourite) || (a[1] - b[1]))
    .map(([f]) => f);
}

/** `ai_aliases` split on commas, trimmed, empties dropped. */
export function parseAliases(s) {
  return String(s || "").split(",").map((a) => a.trim()).filter(Boolean);
}

/**
 * Keep at most `max` lines (blocks). A QPlainTextEdit block is a line; a trailing
 * newline opens an (empty) last block, so "a\nb\n" is 3 blocks.
 */
export function capLines(text, max = OUTPUT_MAX_LINES) {
  let nl = 0;
  for (let i = text.length - 1; i >= 0; i--) {
    if (text.charCodeAt(i) === 10 && ++nl === max) return text.slice(i + 1);
  }
  return text;
}

/** QSpinBox text check: digits only, within `min`-`max`; else null. */
export function parseSpin(text, min, max) {
  if (!/^\s*\d{1,9}\s*$/.test(text)) return null;
  const v = parseInt(text, 10);
  return v >= min && v <= max ? v : null;
}

/** Port spin box value check (QSpinBox range 1024-65535, digits only). */
export function parsePort(text) {
  return /^\s*\d{1,5}\s*$/.test(text) ? parseSpin(text, PORT_MIN, PORT_MAX) : null;
}

/** Selection fix after removing index `idx` (`_remove_folder`). */
export function selectionAfterRemove(selected, length) {
  if (selected !== null && selected >= length) return length ? length - 1 : null;
  return selected;
}

/** `_next_port`: lowest port >= PORT_BASE not used by any folder. */
export function nextPort(folders) {
  const used = new Set(folders.map((f) => f.port));
  let p = PORT_BASE;
  while (used.has(p)) p++;
  return p;
}

/**
 * Ports are unique per folder (deviation from Python, which allowed duplicates: two
 * folders on one port cannot run at the same time, and removing one closed the other's
 * Blender). The folder other than `self` that uses `port`, or null.
 */
export function portOwner(folders, port, self = null) {
  return folders.find((f) => f !== self && f.port === port) ?? null;
}

/**
 * Give every folder a port no earlier folder uses: the first folder keeps a shared port,
 * later ones move to the lowest free port >= PORT_BASE (like `nextPort`). Mutates the
 * folders; returns the moves as [{path, from, to}] (empty when the ports were unique).
 */
export function dedupePorts(folders) {
  const used = new Set(folders.map((f) => f.port));
  const seen = new Set();
  const moves = [];
  for (const f of folders) {
    if (!seen.has(f.port)) {
      seen.add(f.port);
      continue;
    }
    let p = PORT_BASE;
    while (used.has(p) && p <= PORT_MAX) p++;
    if (p > PORT_MAX) continue; // no free port left: keep it, the launch will refuse
    moves.push({ path: f.path, from: f.port, to: p });
    f.port = p;
    used.add(p);
    seen.add(p);
  }
  return moves;
}

/** Message after `dedupePorts` moved folders. */
export function dedupeText(moves) {
  const lines = moves.map((m) => `  ${m.path}: ${m.from} → ${m.to}`);
  return `Each folder needs its own port. These folders shared a port and were moved:\n\n${lines.join("\n")}`;
}

/** Edit / port box: refusal text when `port` belongs to another folder. */
export function portTakenText(port, owner) {
  return `Port ${port} is already used by:\n${owner.path}\n\nPick another port.`;
}

/**
 * Path key for "already added" checks: `str(Path(p).resolve())` compared on Windows,
 * where resolve() also yields the on-disk case. Separators unified, `.`/`..` resolved,
 * trailing separator dropped, case folded. (Symlinks/junctions are not resolved.)
 */
export function pathKey(p) {
  let s = String(p).trim().replace(/\//g, "\\");
  const unc = s.startsWith("\\\\");
  s = s.replace(/\\+/g, "\\");
  const parts = [];
  for (const part of s.split("\\")) {
    if (part === "" || part === ".") continue;
    if (part === "..") {
      if (parts.length > 1 || (parts.length === 1 && !/^[a-z]:$/i.test(parts[0]))) parts.pop();
      continue;
    }
    parts.push(part);
  }
  let out = parts.join("\\");
  if (/^[a-z]:$/i.test(out)) out += "\\";
  return ((unc ? "\\\\" : s.startsWith("\\") ? "\\" : "") + out).toLowerCase();
}

/** ScanAddonsDialog item text: `f"[{kind:<9}]  {path}"` (+ "    (already added)"). */
export function addonLabel(kind, path, already) {
  const text = `[${String(kind).padEnd(9)}]  ${path}`;
  return already ? `${text}    (already added)` : text;
}

/**
 * ScanAddonsDialog items for `found` ([{path, kind}] from scan_addons): already added
 * ones are disabled and unchecked, the others checked.
 */
export function scanItems(found, existingFolders) {
  const existing = new Set(existingFolders.map((f) => pathKey(f.path)));
  return found.map(({ path, kind }) => {
    const already = existing.has(pathKey(path));
    return { path, kind, already, enabled: !already, checked: !already, label: addonLabel(kind, path, already) };
  });
}

/** `ScanAddonsDialog._ok`: the checked items, in list order. */
export function checkedAddons(items) {
  return items.filter((i) => i.checked).map(({ path, kind }) => ({ path, kind }));
}

/** `_set_all`: check or uncheck every enabled item. */
export function setAllChecked(items, checked) {
  for (const i of items) if (i.enabled) i.checked = checked;
}

/** `Folder({"path": path, "port": port})` with the Folder defaults. */
export function newFolder(path, port) {
  return { path: String(path), port, repo: "blinker", module: "", blend: "", favourite: false };
}

/**
 * `_add_folder` tail: append one folder per path, each on `_next_port()` at the time
 * it is added. Mutates `folders`; returns the new selected index (the last added),
 * or null when nothing was added.
 */
export function appendFolders(folders, paths) {
  if (!paths.length) return null;
  for (const p of paths) folders.push(newFolder(p, nextPort(folders)));
  return folders.length - 1;
}

/** EditFolderDialog.accept: the folder fields from the dialog's field texts. */
export function editResult({ path, port, repo, module, blend }) {
  return {
    path: String(path).trim(),
    port,
    repo: String(repo).trim() || "blinker",
    module: String(module).trim(),
    blend: String(blend).trim(),
  };
}

/** QSpinBox.value() at accept: the typed text if it is a valid port, else the last value. */
export function spinValue(text, last) {
  return parsePort(text) ?? last;
}

/** EditFolderDialog._browse_blend start directory: blend, else path, else "". */
export function blendStart(blend, path) {
  return blend || path || "";
}

// ---------- Settings dialog (SettingsDialog, blinker_ui.py:1835-2212) ----------

export const FONT_SIZE_MIN = 7; // QSpinBox.setRange(7, 24)
export const FONT_SIZE_MAX = 24;

/**
 * `_commit_inputs`: the dialog's result from its draft theme and field values.
 * `defaults` = { theme, terminal } (DEFAULT_THEME, DEFAULT_TERMINAL).
 */
export function commitSettings(draft, f, defaults) {
  return {
    theme: {
      ...draft,
      ui_font_family: f.uiFamily || defaults.theme.ui_font_family,
      ui_font_size: f.uiSize,
      mono_font_family: f.monoFamily || defaults.theme.mono_font_family,
      mono_font_size: f.monoSize,
    },
    terminal: String(f.terminal).trim() || defaults.terminal,
    ai_aliases: String(f.aiAliases).trim(),
    blender_path: String(f.blenderPath).trim(),
  };
}

/** `_import_theme` tail: the "Import notes" text, or null when there is nothing to say. */
export function importNotes(unknown, invalid) {
  const msgs = [];
  if (unknown.length) msgs.push(`Ignored unknown keys: ${unknown.join(", ")}`);
  if (invalid.length) msgs.push(`Invalid values for: ${invalid.join(", ")}`);
  return msgs.length ? msgs.join("\n") : null;
}

/**
 * QComboBox inline completion (QCompleter::InlineCompletion, case-insensitive): the
 * first item that starts with `typed`, or null.
 */
export function completeFrom(items, typed) {
  if (!typed) return null;
  const t = typed.toLowerCase();
  return items.find((i) => i.toLowerCase().startsWith(t)) ?? null;
}

/**
 * Output events that arrived while `get_output` was in flight, reduced to the text
 * that is not in the snapshot: events whose `seq` is past the snapshot's, in seq order.
 * (The backend stamps each event with the console sequence after that text.)
 */
export function textAfterSnapshot(snapshotSeq, buffered) {
  return buffered
    .filter((e) => e.seq > snapshotSeq)
    .sort((a, b) => a.seq - b.seq)
    .map((e) => e.text)
    .join("");
}

/** Running state per folder port from a full `blinker://status` payload; missing = false. */
export function runningFromStatus(ports, payload) {
  return new Map(ports.map((p) => [p, payload[String(p)] === true]));
}

/**
 * Serialises saves: at most one in flight; requests made meanwhile coalesce into one
 * follow-up save, which sends `current()` as it is when that save starts (never an
 * older snapshot). `send(value)` returns a promise. The promise from `request()`
 * resolves once a save that started after the request has finished.
 */
export class SaveQueue {
  constructor(send, current, onError = () => {}) {
    this.send = send;
    this.current = current;
    this.onError = onError;
    this.busy = false;
    this.dirty = false;
    this.paused = false;
    this.waiters = [];
    this.idle = [];
  }

  /**
   * Stop sending: resolves once the save in flight (if any) has finished. Requests made
   * while paused stay pending (`dirty`) until `resume()` or `discard()`.
   */
  async pause() {
    this.paused = true;
    while (this.busy) await new Promise((r) => this.idle.push(r));
  }

  /** Continue; a request made while paused is sent now. */
  resume() {
    this.paused = false;
    if (this.dirty && !this.busy) this._pump();
  }

  /** Drop pending requests (their waiters resolve) and continue. */
  discard() {
    this.dirty = false;
    const w = this.waiters;
    this.waiters = [];
    for (const r of w) r();
    this.paused = false;
  }

  request() {
    this.dirty = true;
    const done = new Promise((r) => this.waiters.push(r));
    if (!this.busy && !this.paused) this._pump();
    return done;
  }

  async _pump() {
    this.busy = true;
    while (this.dirty && !this.paused) {
      this.dirty = false;
      const waiters = this.waiters;
      this.waiters = [];
      try {
        await this.send(this.current());
      } catch (e) {
        this.onError(e);
      }
      for (const r of waiters) r();
    }
    this.busy = false;
    const idle = this.idle;
    this.idle = [];
    for (const r of idle) r();
  }
}

/**
 * Console feed for the selected port. While a snapshot (`get_output`) or a clear is in
 * flight, events are buffered; `end(seq)` returns the buffered text past `seq` and makes
 * `seq` the threshold. Live events are then shown iff their seq is past that threshold,
 * in arrival order: an event is never dropped because ANOTHER event had a larger seq
 * (concurrent emitters on one port can deliver out of seq order).
 */
export class ConsoleFeed {
  constructor() {
    this.threshold = 0;
    this.buffer = null;
  }

  begin() {
    this.buffer = [];
  }

  end(seq) {
    const text = textAfterSnapshot(seq, this.buffer || []);
    this.buffer = null;
    this.threshold = seq;
    return text;
  }

  /** Text to append now for a live event ("" when buffered or already shown). */
  event(seq, text) {
    if (this.buffer) {
      this.buffer.push({ seq, text });
      return "";
    }
    return seq > this.threshold ? text : "";
  }
}

const plural = (n, one, many) => `${n} ${n === 1 ? one : many}`;

/** "Imported 7 folders." / "Imported 1 folder." */
export function importDoneText(n) {
  return `Imported ${plural(n, "folder", "folders")}.`;
}

/**
 * The Import Config confirm text from `inspect_config_file`'s summary
 * ({folders, skipped_folders, has_theme, has_terminal, has_ai_aliases, has_blender_path,
 * sample_paths}) and the number of folders in the CURRENT setup, which the import replaces.
 */
export function importConfirmText(path, s, currentFolders = 0) {
  const parts = [plural(s.folders, "folder", "folders")];
  if (s.has_terminal) parts.push("terminal");
  if (s.has_ai_aliases) parts.push("AI aliases");
  if (s.has_blender_path) parts.push("Blender path");
  if (s.has_theme) parts.push("theme");
  const lines = ["Import this config?", "", path, "", parts.join(", ")];
  const sample = s.sample_paths || [];
  if (sample.length) {
    const label = s.folders > sample.length ? "first folders" : sample.length === 1 ? "folder" : "folders";
    lines.push(`(${label}: ${sample.join(", ")})`);
  }
  if (s.folders === 0 && currentFolders > 0) {
    lines.push("", `This file has no folders: all your current ${plural(currentFolders, "folder", "folders")} will be removed.`);
  }
  if (s.skipped_folders > 0) {
    lines.push("", s.skipped_folders === 1
      ? "1 unreadable folder entry is kept in the file but not shown"
      : `${s.skipped_folders} unreadable folder entries are kept in the file but not shown`);
  }
  const missing = [];
  if (!s.has_terminal) missing.push("terminal");
  if (!s.has_ai_aliases) missing.push("AI aliases");
  if (!s.has_blender_path) missing.push("Blender path");
  if (!s.has_theme) missing.push("theme");
  if (missing.length) lines.push("", `Not in this file (reset to defaults): ${missing.join(", ")}`);
  const replaced = currentFolders > 0
    ? `This replaces your current ${plural(currentFolders, "folder", "folders")} and settings.`
    : "This replaces your current settings.";
  lines.push("", `${replaced} The current config is saved as blinker_ui_config.json.bak.`);
  return lines.join("\n");
}

/**
 * Replace the setup while saves are held back: waits for the save in flight, then
 * `fetchNew()` (the backend call) and `swap(value)` (synchronous: install the value in the
 * UI state). When both succeed the pending saves are dropped (`discard`: they would put the
 * old setup back); on ANY failure (a rejected call, a throw in `swap`) saving resumes, so
 * the queue never stays paused. Returns what `fetchNew` returned.
 */
export async function replaceWithSavesPaused(queue, fetchNew, swap) {
  let done = false;
  try {
    await queue.pause();
    const value = await fetchNew();
    swap(value);
    done = true;
    return value;
  } finally {
    if (done) queue.discard();
    else queue.resume();
  }
}
