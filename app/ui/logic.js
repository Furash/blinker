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

/** Port spin box value check (QSpinBox range 1024-65535, digits only). */
export function parsePort(text) {
  if (!/^\s*\d{1,5}\s*$/.test(text)) return null;
  const v = parseInt(text, 10);
  return v >= PORT_MIN && v <= PORT_MAX ? v : null;
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
    this.waiters = [];
  }

  request() {
    this.dirty = true;
    const done = new Promise((r) => this.waiters.push(r));
    if (!this.busy) this._pump();
    return done;
  }

  async _pump() {
    this.busy = true;
    while (this.dirty) {
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
