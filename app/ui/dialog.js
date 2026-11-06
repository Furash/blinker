// Minimal themed modal (QMessageBox stand-in). Task 10 extends this into the full
// dialog system; keep the API small: message() and confirm().

function modal(title, text, buttons) {
  return new Promise((resolve) => {
    const prevFocus = document.activeElement;
    const overlay = document.createElement("div");
    overlay.className = "modal-overlay";
    const box = document.createElement("div");
    box.className = "modal";
    box.setAttribute("role", "dialog");
    box.setAttribute("aria-modal", "true");
    const h = document.createElement("div");
    h.className = "modal-title";
    h.textContent = title;
    const body = document.createElement("div");
    body.className = "modal-text";
    body.textContent = text;
    const row = document.createElement("div");
    row.className = "modal-buttons";
    box.append(h, body, row);
    overlay.append(box);

    const close = (value) => {
      document.removeEventListener("keydown", onKey, true);
      overlay.remove();
      if (prevFocus && prevFocus.focus) prevFocus.focus();
      resolve(value);
    };
    const els = buttons.map(([label, value]) => {
      const b = document.createElement("button");
      b.textContent = label;
      b.addEventListener("click", () => close(value));
      row.append(b);
      return b;
    });
    const cancelValue = buttons[buttons.length - 1][1];
    function onKey(e) {
      if (e.key === "Escape") { e.preventDefault(); e.stopPropagation(); close(cancelValue); }
      else if (e.key === "Tab") {
        // keep focus inside the dialog
        e.preventDefault();
        const i = els.indexOf(document.activeElement);
        els[(i + (e.shiftKey ? els.length - 1 : 1)) % els.length].focus();
      }
    }
    document.addEventListener("keydown", onKey, true);
    document.body.append(overlay);
    els[0].focus();
  });
}

/** Information / error box with a single OK button. */
export function message(title, text) {
  return modal(title, text, [["OK", undefined]]);
}

/** Yes/No question; resolves true for Yes. */
export function confirm(title, text) {
  return modal(title, text, [["Yes", true], ["No", false]]);
}

/** Placeholder for toolbar actions that later tasks implement. */
export function notImplemented(title) {
  return message(title, "Not implemented yet");
}
