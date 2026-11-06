// EditFolderDialog (blinker_ui.py:1758-1830). Resolves to the new folder fields
// {path, port, repo, module, blend} on OK (EditFolderDialog.accept: no validation
// besides the spin box range), null on Cancel/Esc. The folder itself is not touched.

import { openDialog, pushButton, buttonBox } from "./dialog.js";
import { portSpin } from "./rows.js";
import { editResult, spinValue, blendStart } from "./logic.js";
import { pickFolder, pickFile } from "./api.js";

const el = (tag, cls, text) => {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
};

function lineEdit(value) {
  const i = el("input", "qline");
  i.type = "text";
  i.spellcheck = false;
  i.autocomplete = "off";
  i.value = value ?? "";
  return i;
}

/**
 * `portLocked`: the folder is running. Like the row's port box (rows.js), the port
 * cannot change while a supervised Blender runs on it (deviation from Python: the Rust
 * Supervisor keys the process by its port).
 */
export function editDialog(folder, { portLocked = false } = {}) {
  const pathE = lineEdit(folder.path);
  const repoE = lineEdit(folder.repo);
  const moduleE = lineEdit(folder.module);
  const blendE = lineEdit(folder.blend);
  const spin = portSpin(folder.port, portLocked, () => {}, "");
  const spinInput = spin.querySelector("input");

  let busy = false; // a native picker is open
  const browse = async (pick, target) => {
    if (busy) return;
    busy = true;
    try {
      const r = await pick();
      if (r) target.value = r;
    } finally {
      busy = false;
    }
  };
  const pathBtn = pushButton("…", () => browse(() => pickFolder("Select addon folder", pathE.value), pathE), { cls: "browse" });
  const blendBtn = pushButton("…", () => browse(() => pickFile(
    "Select .blend",
    [{ name: "Blend files", extensions: ["blend"] }, { name: "All files", extensions: ["*"] }],
    blendStart(blendE.value, pathE.value),
  ), blendE), { cls: "browse" });

  const fieldRow = (input, btn) => {
    const r = el("div", "field-row");
    r.append(input, btn);
    return r;
  };
  const form = el("div", "form");
  form.append(
    el("label", "form-label", "Path"), fieldRow(pathE, pathBtn),
    el("label", "form-label", "Port"), spin,
    el("label", "form-label", "Repo"), repoE,
    el("label", "form-label", "Module"), moduleE,
    el("label", "form-label", "Blend"), fieldRow(blendE, blendBtn),
  );
  const hint = el("div", "meta", "Module/Repo blank = auto. Repo defaults to 'blinker' (extensions only).");

  let dlg;
  const ok = () => {
    if (busy) return;
    dlg.close(editResult({
      path: pathE.value,
      port: spinValue(spinInput.value, Number(spin.dataset.port)),
      repo: repoE.value,
      module: moduleE.value,
      blend: blendE.value,
    }));
  };
  const cancel = () => { if (!busy) dlg.close(null); };
  dlg = openDialog({
    title: "Edit folder",
    cls: "edit",
    width: 680,
    height: 280,
    content: [form, hint, el("div", "stretch"), buttonBox([["OK", ok], ["Cancel", cancel]])],
    onAccept: ok,
    onReject: cancel,
    focus: pathE,
  });
  return dlg.result;
}
