// QComboBox stand-in (editable or not), styled by the app stylesheet's QComboBox rules:
// a line-edit-like box with a 20 px drop-down button, and a popup list (QAbstractItemView:
// input_bg, accent selection) under it.
//
//   comboBox({ items, value, editable, onChange })
//     onChange(text): currentTextChanged for a pick from the list, an arrow/wheel step,
//     and (editable) editingFinished with a changed text.
// Returns the element with .value() / .setValue(text) (setCurrentText, no onChange).
//
// Keys (Qt): Up/Down/PageUp/PageDown/Home/End step through the items when closed;
// Alt+Down or F4 opens; in the popup Up/Down move, Enter picks, Esc closes (the dialog
// does not see that Esc). Editable: inline completion (QCompleter::InlineCompletion,
// case-insensitive), as QComboBox sets up.

import { completeFrom } from "./logic.js";

const el = (tag, cls, text) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
};

const MAX_VISIBLE = 10; // QComboBox.maxVisibleItems

export function comboBox({ items, value = "", editable = false, onChange = () => {} }) {
  let list = [...items];
  const box = el("div", "combo" + (editable ? " editable" : ""));
  let field;
  if (editable) {
    field = el("input", "combo-edit");
    field.type = "text";
    field.spellcheck = false;
    field.autocomplete = "off";
  } else {
    field = el("div", "combo-text");
    box.tabIndex = 0;
  }
  const drop = el("div", "combo-drop");
  box.append(field, drop);

  let text = String(value);
  let committed = text;
  const show = () => {
    if (editable) field.value = text;
    else field.textContent = text;
  };
  show();

  const current = () => (editable ? field.value : text);
  const emit = () => {
    if (current() !== committed) {
      committed = current();
      onChange(committed);
    }
  };
  const setText = (t, fire) => {
    text = t;
    show();
    if (fire) emit();
    else committed = t;
  };

  // ---------- popup ----------
  let popup = null;
  let hi = -1;
  function paintHi() {
    if (!popup) return;
    [...popup.children].forEach((c, i) => c.classList.toggle("hi", i === hi));
    const c = popup.children[hi];
    if (c) {
      if (c.offsetTop < popup.scrollTop) popup.scrollTop = c.offsetTop;
      else if (c.offsetTop + c.offsetHeight > popup.scrollTop + popup.clientHeight) {
        popup.scrollTop = c.offsetTop + c.offsetHeight - popup.clientHeight;
      }
    }
  }
  function open() {
    if (popup) return;
    popup = el("div", "combo-popup");
    list.forEach((it, i) => {
      const r = el("div", "combo-item", it);
      r.addEventListener("mousemove", () => { if (hi !== i) { hi = i; paintHi(); } });
      r.addEventListener("mousedown", (e) => e.preventDefault());
      r.addEventListener("click", () => { close(); setText(it, true); focusField(); });
      popup.append(r);
    });
    document.body.append(popup); // position: fixed, above the modal overlay
    const r = box.getBoundingClientRect();
    popup.style.left = `${r.left}px`;
    popup.style.width = `${r.width}px`;
    const itemH = popup.firstChild ? popup.firstChild.getBoundingClientRect().height : 20;
    popup.style.maxHeight = `${Math.round(itemH * MAX_VISIBLE) + 2}px`;
    const ph = popup.getBoundingClientRect().height;
    // below the box, or above it when it does not fit in the window
    const below = r.bottom + ph <= window.innerHeight || r.top - ph < 0;
    popup.style.top = `${below ? r.bottom : r.top - ph}px`;
    hi = list.findIndex((x) => x === current());
    if (hi < 0 && editable) hi = list.findIndex((x) => x.toLowerCase() === current().toLowerCase());
    paintHi();
    if (hi >= 0) {
      const c = popup.children[hi];
      popup.scrollTop = Math.max(0, c.offsetTop - (popup.clientHeight - c.offsetHeight) / 2);
    }
    window.addEventListener("keydown", popupKey, true);
    document.addEventListener("mousedown", outside, true);
  }
  function close() {
    if (!popup) return;
    popup.remove();
    popup = null;
    window.removeEventListener("keydown", popupKey, true);
    document.removeEventListener("mousedown", outside, true);
  }
  function outside(e) {
    if (popup && !popup.contains(e.target) && !drop.contains(e.target) && (editable || !box.contains(e.target))) close();
  }
  function popupKey(e) {
    const k = e.key;
    const stop = () => { e.preventDefault(); e.stopImmediatePropagation(); };
    if (k === "Escape" || (k === "ArrowUp" && e.altKey) || k === "F4") { stop(); close(); }
    else if (k === "Enter" || k === " " && !editable) { stop(); const it = list[hi]; close(); if (it !== undefined) setText(it, true); }
    else if (k === "ArrowDown") { stop(); hi = Math.min(list.length - 1, hi + 1); paintHi(); }
    else if (k === "ArrowUp") { stop(); hi = Math.max(0, hi - 1); paintHi(); }
    else if (k === "PageDown") { stop(); hi = Math.min(list.length - 1, hi + MAX_VISIBLE - 1); paintHi(); }
    else if (k === "PageUp") { stop(); hi = Math.max(0, hi - (MAX_VISIBLE - 1)); paintHi(); }
    else if (k === "Home") { stop(); hi = 0; paintHi(); }
    else if (k === "End") { stop(); hi = list.length - 1; paintHi(); }
    else if (k === "Tab") { stop(); close(); }
  }
  const focusField = () => (editable ? field : box).focus();

  drop.addEventListener("mousedown", (e) => {
    if (e.button !== 0) return;
    e.preventDefault();
    focusField();
    if (popup) close(); else open();
  });
  if (!editable) {
    box.addEventListener("mousedown", (e) => {
      if (e.button !== 0 || drop.contains(e.target)) return;
      e.preventDefault();
      box.focus();
      if (popup) close(); else open();
    });
  }

  // ---------- keys / wheel when closed ----------
  const stepTo = (i) => {
    if (!list.length) return;
    let cur = list.indexOf(current());
    if (cur < 0) cur = i > 0 ? -1 : list.length;
    const n = Math.max(0, Math.min(list.length - 1, i === Infinity ? list.length - 1 : i === -Infinity ? 0 : cur + i));
    if (list[n] !== current()) setText(list[n], true);
  };
  (editable ? field : box).addEventListener("keydown", (e) => {
    if (popup) return;
    const k = e.key;
    if ((k === "ArrowDown" && e.altKey) || k === "F4") { e.preventDefault(); open(); }
    else if (k === "ArrowDown") { e.preventDefault(); stepTo(1); }
    else if (k === "ArrowUp") { e.preventDefault(); stepTo(-1); }
    else if (k === "PageDown") { e.preventDefault(); stepTo(MAX_VISIBLE - 1); }
    else if (k === "PageUp") { e.preventDefault(); stepTo(-(MAX_VISIBLE - 1)); }
    else if (!editable && k === "Home") { e.preventDefault(); stepTo(-Infinity); }
    else if (!editable && k === "End") { e.preventDefault(); stepTo(Infinity); }
    else if (!editable && k === " ") { e.preventDefault(); open(); }
    else if (editable && k === "Enter") emit();
  });
  box.addEventListener("wheel", (e) => {
    e.preventDefault();
    if (popup) return;
    stepTo(e.deltaY < 0 ? -1 : 1);
  }, { passive: false });

  if (editable) {
    field.addEventListener("input", (e) => {
      text = field.value;
      // inline completion on insertion at the end of the text
      if (!e.inputType || !e.inputType.startsWith("insert")) return;
      if (field.selectionStart !== field.value.length) return;
      const typed = field.value;
      const c = completeFrom(list, typed);
      if (c && c.length > typed.length) {
        field.value = c; // the completion's own case, the untyped part selected
        text = field.value;
        field.setSelectionRange(typed.length, field.value.length);
      }
    });
    field.addEventListener("blur", () => { if (!popup) emit(); });
  }

  box.value = current;
  box.setValue = (t) => setText(String(t), false);
  /** Replace the items (the current text is kept). */
  box.setItems = (next) => { list = [...next]; };
  box.closePopup = close;
  return box;
}
