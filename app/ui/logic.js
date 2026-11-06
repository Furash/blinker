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
