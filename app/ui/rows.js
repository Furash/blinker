// Folder rows (FolderRow, blinker_ui.py:854-1028).
// Visual order is Qt's layout order: dot, path, meta, stretch, "port", spin box,
// [btn_row: Launch/Kill, Reload, Restart, Clear console, │, AI aliases], fav,
// Open folder (new), Edit, ✕.

import { metaText, parseAliases, parseSpin, PORT_MIN, PORT_MAX } from "./logic.js";

const el = (tag, cls, text) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
};

function button(text, onClick, opts = {}) {
  const b = el("button", opts.cls, text);
  b.type = "button";
  if (opts.title) b.title = opts.title;
  if (opts.disabled) b.disabled = true;
  b.addEventListener("click", onClick);
  return b;
}

/**
 * QSpinBox stand-in: digits, `min`-`max`, arrows/wheel/keys step by 1. `onCommit(v)`
 * runs on editingFinished with a changed value; `onStep(v)` after each arrow/wheel/key
 * step (valueChanged); `wrap.dataset.value` is the last committed value.
 */
export function spinBox({ value, min, max, disabled = false, onCommit = () => {}, onStep = () => {}, title = "" }) {
  const wrap = el("div", "spin");
  if (title) wrap.title = title;
  const input = el("input");
  input.type = "text";
  input.inputMode = "numeric";
  input.maxLength = String(max).length;
  input.spellcheck = false;
  input.value = String(value);
  const up = el("button", "spin-up");
  const down = el("button", "spin-down");
  up.type = down.type = "button";
  up.tabIndex = down.tabIndex = -1;
  wrap.append(input, up, down);
  setSpinDisabled(wrap, disabled);

  const parse = (t) => parseSpin(t, min, max);
  wrap.dataset.value = String(value); // last committed value
  const step = (d) => {
    const v = parse(input.value) ?? Number(wrap.dataset.value);
    input.value = String(Math.min(max, Math.max(min, v + d)));
    onStep(Number(input.value));
  };
  // editingFinished: Enter or focus loss; emit only when the value changed.
  const commit = () => {
    const v = parse(input.value);
    if (v === null) {
      input.value = wrap.dataset.value;
      return;
    }
    input.value = String(v);
    if (String(v) !== wrap.dataset.value) {
      wrap.dataset.value = String(v);
      onCommit(v);
    }
  };
  /** QSpinBox.setValue (clamped to the range), without onCommit. */
  wrap.setValue = (v) => {
    const c = String(Math.min(max, Math.max(min, Math.trunc(Number(v)))));
    input.value = c;
    wrap.dataset.value = c;
  };
  /** QSpinBox.value(): the typed value if valid, else the last value. */
  wrap.value = () => parse(input.value) ?? Number(wrap.dataset.value);
  input.addEventListener("beforeinput", (e) => {
    if (e.data && /\D/.test(e.data)) e.preventDefault();
  });
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") commit();
    else if (e.key === "ArrowUp") { e.preventDefault(); step(1); }
    else if (e.key === "ArrowDown") { e.preventDefault(); step(-1); }
    else if (e.key === "PageUp") { e.preventDefault(); step(10); }
    else if (e.key === "PageDown") { e.preventDefault(); step(-10); }
  });
  input.addEventListener("blur", commit);
  wrap.addEventListener("wheel", (e) => {
    if (input.disabled) return;
    e.preventDefault();
    if (document.activeElement !== input) input.focus();
    step(e.deltaY < 0 ? 1 : -1);
  }, { passive: false });
  for (const [b, d] of [[up, 1], [down, -1]]) {
    let timer = 0;
    const stop = () => { clearTimeout(timer); clearInterval(timer); };
    b.addEventListener("mousedown", (e) => {
      if (e.button !== 0) return;
      e.preventDefault();
      input.focus();
      step(d);
      timer = setTimeout(() => { timer = setInterval(() => step(d), 60); }, 300);
    });
    b.addEventListener("mouseup", stop);
    b.addEventListener("mouseleave", stop);
  }
  return wrap;
}

/** The row's port box (1024-65535); `wrap.dataset.port` mirrors the committed value. */
export function portSpin(value, disabled, onCommit, title = "TCP port the bootstrap server listens on") {
  const wrap = spinBox({
    value, min: PORT_MIN, max: PORT_MAX, disabled, title,
    onCommit: (v) => { wrap.dataset.port = String(v); onCommit(v); },
  });
  wrap.dataset.port = String(value);
  return wrap;
}

// Deviation from Python: the port cannot be edited while the folder is running.
// The Rust Supervisor keys a live Blender by its port, so moving the folder to a
// new port would orphan the running process from Kill/Reload/output.
function setSpinDisabled(wrap, disabled) {
  wrap.classList.toggle("disabled", disabled);
  for (const c of wrap.querySelectorAll("input, button")) c.disabled = disabled;
}

/** The nested btn_row layout (`FolderRow._build_buttons`). */
function buttonRow(idx, running, launching, aliases, icons, h) {
  const btns = el("div", "btn-row");
  btns.append(
    // Launch stays disabled while its `launch` call is in flight (Python flips to Kill
    // synchronously; here Kill arrives with the backend's status event).
    running ? button("Kill", () => h.kill(idx)) : button("Launch", () => h.launch(idx), { disabled: launching }),
    button("Reload", () => h.reload(idx), { disabled: !running }),
    button("Restart", () => h.restart(idx), { disabled: !running }),
    button("Clear console", () => h.clear(idx)),
  );
  const list = parseAliases(aliases);
  if (list.length) {
    btns.append(el("span", "divider", "│"));
    for (const ai of list) {
      const icon = icons.get(ai.toLowerCase());
      if (icon) {
        const b = button("", () => h.ai(idx, ai), { cls: "icon-btn", title: ai });
        const img = el("img");
        img.src = icon;
        img.alt = "";
        img.draggable = false;
        b.append(img);
        btns.append(b);
      } else {
        btns.append(button(ai, () => h.ai(idx, ai)));
      }
    }
  }
  return btns;
}

function favButton(idx, fav, h) {
  return button(fav ? "★" : "☆", () => h.fav(idx), {
    cls: "fav-btn" + (fav ? " on" : ""),
    title: fav ? "Unpin from top" : "Pin to top",
  });
}

/**
 * Build one row. `h` holds the callbacks (all take the folder index):
 * select, launch, kill, reload, restart, clear, openFolder, edit, remove, fav,
 * port(idx, value), ai(idx, alias). `icons` maps alias -> data URI or null.
 */
export function buildRow(folder, idx, ctx) {
  const { selected, running, launching, aliases, icons, h } = ctx;
  const row = el("div", "row" + (selected ? " selected" : ""));
  row.dataset.idx = String(idx);
  // FolderRow.mousePressEvent: a press anywhere that no child widget takes selects.
  row.addEventListener("mousedown", (e) => {
    if (e.target.closest("button, .spin")) return;
    h.select(idx);
  });

  const dot = el("span", "dot" + (running ? " running" : ""), running ? "●" : "○");
  const path = el("span", "path", folder.path);
  const meta = el("span", "meta", metaText(folder));
  const portLbl = el("span", "meta port-label", "port");
  const spin = portSpin(folder.port, running, (v) => h.port(idx, v));
  const btns = buttonRow(idx, running, launching, aliases, icons, h);

  row.append(
    dot, path, meta, portLbl, spin, btns,
    favButton(idx, !!folder.favourite, h),
    button("Open folder", () => h.openFolder(idx), { title: "Open folder in Explorer" }),
    button("Edit", () => h.edit(idx)),
    button("✕", () => h.remove(idx), { cls: "remove-btn" }),
  );
  return row;
}

/** Re-render the whole list (`_refresh_rows`). */
export function renderRows(listEl, state, h) {
  const folders = state.config.folders;
  const frag = document.createDocumentFragment();
  if (!folders.length) {
    frag.append(el("div", "meta empty", "No folders yet. Click '+ Add Folder' to start."));
  }
  folders.forEach((f, i) => {
    frag.append(buildRow(f, i, {
      selected: i === state.selected,
      running: !!state.running.get(f.port),
      launching: state.launching.has(f.port),
      aliases: state.config.ai_aliases,
      icons: state.icons,
      h,
    }));
  });
  listEl.replaceChildren(frag);
}

/**
 * Refresh row `i` in place (`FolderRow.update_state`): dot, labels, port, fav and the
 * button row are updated; the spin box element is kept so an edit in progress survives.
 */
export function updateRow(listEl, state, i, h) {
  const row = listEl.querySelector(`.row[data-idx="${i}"]`);
  const f = state.config.folders[i];
  if (!row || !f) return;
  const running = !!state.running.get(f.port);
  const dot = row.querySelector(".dot");
  dot.textContent = running ? "●" : "○";
  dot.classList.toggle("running", running);
  row.querySelector(".path").textContent = f.path;
  row.querySelector(".meta:not(.port-label)").textContent = metaText(f);
  const spin = row.querySelector(".spin");
  const input = spin.querySelector("input");
  spin.dataset.port = spin.dataset.value = String(f.port);
  if (document.activeElement !== input) input.value = String(f.port);
  setSpinDisabled(spin, running);
  row.querySelector(".btn-row").replaceWith(buttonRow(i, running, state.launching.has(f.port), state.config.ai_aliases, state.icons, h));
  row.querySelector(".fav-btn").replaceWith(favButton(i, !!f.favourite, h));
}

/** Toggle the selected class (`FolderRow.set_selected`). */
export function markSelected(listEl, idx) {
  for (const r of listEl.querySelectorAll(".row")) {
    r.classList.toggle("selected", Number(r.dataset.idx) === idx);
  }
}
