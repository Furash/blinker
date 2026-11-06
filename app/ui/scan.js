// ScanAddonsDialog (blinker_ui.py:1678-1754): checkable list of the addons found under
// a folder; already added ones are disabled. Resolves to the checked [{path, kind}]
// (possibly empty) on OK, null on Cancel/Esc.

import { openDialog, pushButton, buttonBox } from "./dialog.js";
import { scanItems, checkedAddons, setAllChecked } from "./logic.js";

const el = (tag, cls, text) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
};

/**
 * `root`: the scanned folder as shown; `found`: scan_addons result; `folders`: the
 * current folder list (for "already added").
 */
export function scanDialog(root, found, folders) {
  const items = scanItems(found, folders);
  let current = -1; // QListWidget current/selected row

  const header = el("div", "meta", `Found ${found.length} addon(s) under:\n${root}`);
  header.style.whiteSpace = "pre";
  const list = el("div", "qlist");
  list.tabIndex = 0;
  list.setAttribute("role", "listbox");
  const inner = el("div", "qlist-items");
  list.append(inner);

  const rows = items.map((it, i) => {
    const row = el("div", "qitem");
    row.setAttribute("role", "option");
    const ind = el("span", "ind");
    row.append(ind, el("span", "label", it.label));
    // QStyledItemDelegate: a release on the indicator toggles; a press selects.
    row.addEventListener("mousedown", (e) => {
      if (e.button !== 0) return;
      e.preventDefault();
      list.focus();
      if (it.enabled) setCurrent(i);
    });
    row.addEventListener("click", (e) => {
      if (e.target === ind && it.enabled) toggle(i);
    });
    // itemDoubleClicked -> _toggle_item (a double click on the indicator is eaten by
    // the delegate, so there the two clicks' toggles cancel out, as in Qt).
    row.addEventListener("dblclick", (e) => {
      if (e.target !== ind && it.enabled) toggle(i);
    });
    inner.append(row);
    return row;
  });

  function paint(i) {
    const it = items[i];
    const r = rows[i];
    r.classList.toggle("checked", it.checked);
    r.classList.toggle("disabled", !it.enabled);
    r.classList.toggle("selected", i === current);
    r.setAttribute("aria-checked", String(it.checked));
    r.setAttribute("aria-disabled", String(!it.enabled));
  }
  const paintAll = () => items.forEach((_, i) => paint(i));

  function setCurrent(i) {
    const prev = current;
    current = i;
    if (prev >= 0) paint(prev);
    paint(i);
    rows[i].scrollIntoView({ block: "nearest" });
  }
  function toggle(i) {
    items[i].checked = !items[i].checked;
    paint(i);
  }

  // Arrow keys move among enabled items; Space toggles the current one.
  list.addEventListener("keydown", (e) => {
    const enabled = items.map((it, i) => (it.enabled ? i : -1)).filter((i) => i >= 0);
    if (!enabled.length) return;
    const pos = enabled.indexOf(current);
    let next = null;
    if (e.key === "ArrowDown") next = enabled[pos < 0 ? 0 : Math.min(enabled.length - 1, pos + 1)];
    else if (e.key === "ArrowUp") next = enabled[pos < 0 ? 0 : Math.max(0, pos - 1)];
    else if (e.key === "Home") next = enabled[0];
    else if (e.key === "End") next = enabled[enabled.length - 1];
    else if (e.key === " ") {
      e.preventDefault();
      if (current >= 0) toggle(current);
      return;
    }
    if (next !== null) {
      e.preventDefault();
      setCurrent(next);
    }
  });

  const setAll = (checked) => {
    setAllChecked(items, checked);
    paintAll();
  };
  const selRow = el("div", "sel-row");
  selRow.append(pushButton("Select all", () => setAll(true)), pushButton("Select none", () => setAll(false)));

  let dlg;
  const ok = () => dlg.close(checkedAddons(items));
  const cancel = () => dlg.close(null);
  paintAll();
  dlg = openDialog({
    title: "Add addons",
    cls: "scan",
    width: 780,
    height: 520,
    content: [header, list, selRow, buttonBox([["OK", ok], ["Cancel", cancel]])],
    onAccept: ok,
    onReject: cancel,
    focus: list,
  });
  return dlg.result;
}
