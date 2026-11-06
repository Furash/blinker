// Themed modal dialogs (QDialog / QMessageBox stand-ins).
//
// Qt dialogs are separate native windows over the main window: no dimming, an OS title
// bar, the client area styled by the app stylesheet. Here a dialog is an in-page frame:
// a Windows 11 style title bar (dark/light following the OS, as Qt's native one does),
// then the client area (`.modal`) laid out like the Qt widgets.
//
// Keyboard (QDialog): Esc rejects (top-most dialog only), Enter presses the default
// button unless focus is on another button (that button is pressed, as with Qt's
// autoDefault push buttons), Tab/Shift+Tab cycle inside the dialog.

const el = (tag, cls, text) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
};

const FOCUSABLE = "button, input, [tabindex]";

function focusables(root) {
  return [...root.querySelectorAll(FOCUSABLE)].filter(
    (e) => !e.disabled && e.tabIndex >= 0 && e.getClientRects().length > 0,
  );
}

const isTop = (overlay) => {
  const all = document.querySelectorAll(".modal-overlay");
  return all[all.length - 1] === overlay;
};

/** A QPushButton for a dialog's client area. */
export function pushButton(text, onClick, opts = {}) {
  const b = el("button", "qbtn" + (opts.cls ? ` ${opts.cls}` : ""), text);
  b.type = "button";
  if (opts.title) b.title = opts.title;
  b.addEventListener("click", onClick);
  return b;
}

/**
 * Open a modal dialog.
 *   title     window title
 *   content   element(s) for the client area (`.modal`, padding 11, spacing 6)
 *   cls       extra class on the client area
 *   width/height  client size in px (QDialog.resize); omitted = natural size
 *   onAccept  Enter with focus outside a button (default button)
 *   onReject  Esc and the title bar close button; default: close(undefined)
 *   focus     element to focus first (default: first focusable, Qt's tab order)
 * Returns { close(value), result: Promise, client }.
 */
export function openDialog({ title, content, cls, width, height, onAccept, onReject, focus }) {
  let resolve;
  const result = new Promise((r) => { resolve = r; });
  const prevFocus = document.activeElement;

  const overlay = el("div", "modal-overlay");
  const frame = el("div", "dlg");
  frame.setAttribute("role", "dialog");
  frame.setAttribute("aria-modal", "true");
  frame.setAttribute("aria-label", title);
  const bar = el("div", "dlg-titlebar");
  const caption = el("span", "dlg-caption", title);
  const x = el("button", "dlg-close");
  x.type = "button";
  x.tabIndex = -1;
  x.title = "Close";
  bar.append(caption, x);
  const client = el("div", "modal" + (cls ? ` ${cls}` : ""));
  if (width) client.style.width = `${width}px`;
  if (height) client.style.height = `${height}px`;
  client.append(...[].concat(content));
  frame.append(bar, client);
  overlay.append(frame);

  let closed = false;
  const close = (value) => {
    if (closed) return;
    closed = true;
    document.removeEventListener("keydown", onKey, true);
    overlay.remove();
    if (prevFocus && prevFocus.focus) prevFocus.focus();
    resolve(value);
  };
  const reject = () => (onReject ? onReject() : close(undefined));
  x.addEventListener("click", reject);

  function onKey(e) {
    // Only the top-most modal handles keys; listeners run oldest first.
    if (!isTop(overlay)) return;
    if (e.key === "Escape") {
      e.preventDefault();
      e.stopImmediatePropagation();
      reject();
    } else if (e.key === "Enter") {
      const t = document.activeElement;
      if (t instanceof HTMLButtonElement && frame.contains(t)) return; // native click
      e.preventDefault();
      e.stopImmediatePropagation();
      if (onAccept) onAccept();
    } else if (e.key === "Tab") {
      e.preventDefault();
      e.stopImmediatePropagation();
      const els = focusables(client);
      if (!els.length) return;
      const i = els.indexOf(document.activeElement);
      const next = i < 0 ? (e.shiftKey ? els.length - 1 : 0) : (i + (e.shiftKey ? els.length - 1 : 1)) % els.length;
      els[next].focus();
      if (els[next] instanceof HTMLInputElement) els[next].select(); // Qt: Tab focus selects all
    }
  }
  document.addEventListener("keydown", onKey, true);
  // Clicks outside the dialog do nothing (modal); keep focus inside.
  overlay.addEventListener("mousedown", (e) => {
    if (e.target === overlay) e.preventDefault();
  });
  frame.addEventListener("focusout", () => {
    requestAnimationFrame(() => {
      if (!closed && !frame.contains(document.activeElement) && isTop(overlay)) {
        (focusables(client)[0] || frame).focus();
      }
    });
  });
  dragByTitle(bar, frame);

  document.body.append(overlay);
  const first = focus || focusables(client)[0];
  if (first) first.focus();
  else { frame.tabIndex = -1; frame.focus(); }
  return { close, result, client };
}

/** Move the dialog by dragging its title bar (kept inside the window). */
function dragByTitle(bar, frame) {
  let dx = 0;
  let dy = 0;
  bar.addEventListener("pointerdown", (e) => {
    if (e.button !== 0 || e.target.closest(".dlg-close")) return;
    e.preventDefault();
    bar.setPointerCapture(e.pointerId);
    const x0 = e.clientX - dx;
    const y0 = e.clientY - dy;
    const r = frame.getBoundingClientRect();
    const baseL = r.left - dx;
    const baseT = r.top - dy;
    const move = (ev) => {
      const maxX = window.innerWidth - r.width - baseL;
      const maxY = window.innerHeight - 30 - baseT;
      dx = Math.min(maxX, Math.max(-baseL, ev.clientX - x0));
      dy = Math.min(maxY, Math.max(-baseT, ev.clientY - y0));
      frame.style.transform = `translate(${dx}px, ${dy}px)`;
    };
    const up = () => {
      bar.removeEventListener("pointermove", move);
      bar.removeEventListener("pointerup", up);
      bar.removeEventListener("pointercancel", up);
    };
    bar.addEventListener("pointermove", move);
    bar.addEventListener("pointerup", up);
    bar.addEventListener("pointercancel", up);
  });
}

/**
 * QDialogButtonBox: right-aligned buttons. `buttons` = [[label, onClick], ...] in
 * Windows order (OK, Cancel). Returns the row element.
 */
export function buttonBox(buttons) {
  const row = el("div", "dlg-buttons");
  for (const [label, onClick] of buttons) row.append(pushButton(label, onClick));
  return row;
}

// ---------- QMessageBox ----------

const ICONS = {
  // Windows 11 shell icons (imageres), redrawn as 48 px SVG.
  information:
    "<svg viewBox='0 0 48 48'><defs><linearGradient id='g' x1='0' y1='0' x2='1' y2='1'><stop offset='0' stop-color='#1d9be3'/><stop offset='1' stop-color='#0369c4'/></linearGradient></defs>"
    + "<circle cx='24' cy='24' r='23.5' fill='url(#g)'/><circle cx='24' cy='13' r='2.4' fill='#fff'/><rect x='22.6' y='18' width='2.8' height='18' rx='1.2' fill='#fff'/></svg>",
  question:
    "<svg viewBox='0 0 48 48'><defs><linearGradient id='g' x1='0' y1='0' x2='1' y2='1'><stop offset='0' stop-color='#1d9be3'/><stop offset='1' stop-color='#0369c4'/></linearGradient></defs>"
    + "<circle cx='24' cy='24' r='23.5' fill='url(#g)'/><path d='M17.5 18.5a6.5 6.5 0 1 1 9.4 5.8c-1.9 1-2.9 2.2-2.9 4.2v1.5' fill='none' stroke='#fff' stroke-width='2.8' stroke-linecap='round'/><circle cx='24' cy='35.5' r='2.1' fill='#fff'/></svg>",
  warning:
    "<svg viewBox='0 0 48 48'><path d='M21.4 4.6a3 3 0 0 1 5.2 0l19 33.4A3 3 0 0 1 43 42.5H5a3 3 0 0 1-2.6-4.5z' fill='#f7c948'/><rect x='22.6' y='14' width='2.8' height='16' rx='1.2' fill='#1f1f1f'/><circle cx='24' cy='35' r='2.1' fill='#1f1f1f'/></svg>",
  critical:
    "<svg viewBox='0 0 48 48'><defs><linearGradient id='g' x1='0' y1='0' x2='1' y2='1'><stop offset='0' stop-color='#e8473c'/><stop offset='1' stop-color='#c42b1c'/></linearGradient></defs>"
    + "<circle cx='24' cy='24' r='23.5' fill='url(#g)'/><path d='M16.5 16.5l15 15m0-15l-15 15' stroke='#fff' stroke-width='2.8' stroke-linecap='round'/></svg>",
};

function messageBox(title, text, buttons, icon) {
  let dlg;
  const row = el("div", "msg-row");
  if (icon && ICONS[icon]) {
    const img = el("img", "msg-icon");
    img.alt = "";
    img.draggable = false;
    img.src = `data:image/svg+xml,${encodeURIComponent(ICONS[icon].replace("<svg ", "<svg xmlns='http://www.w3.org/2000/svg' "))}`;
    row.append(img);
  }
  row.append(el("div", "modal-text", text));
  const box = el("div", "dlg-buttons");
  const els = buttons.map(([label, value]) => {
    const b = pushButton(label, () => dlg.close(value));
    box.append(b);
    return b;
  });
  // QMessageBox: Esc = the reject/no button (the only one for a plain OK box).
  const cancelValue = buttons[buttons.length - 1][1];
  dlg = openDialog({
    title,
    content: [row, box],
    cls: "msgbox",
    onAccept: () => dlg.close(buttons[0][1]),
    onReject: () => dlg.close(cancelValue),
    focus: els[0],
  });
  return dlg.result;
}

/** Message box with a single OK button; `icon`: information | warning | critical. */
export function message(title, text, icon = null) {
  return messageBox(title, text, [["OK", undefined]], icon);
}

/**
 * Yes/No question (QMessageBox.question); resolves true for Yes. `icon` "warning" is
 * QMessageBox.warning with Yes|No buttons.
 */
export function confirm(title, text, icon = "question") {
  return messageBox(title, text, [["Yes", true], ["No", false]], icon);
}
