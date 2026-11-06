// Thin wrapper over the Tauri global API. Falls back to mock.js in a plain browser.

const T = window.__TAURI__;
const mock = T ? null : await import("./mock.js");

export const isMock = !T;
export const mockScenario = mock ? mock.scenario : null;

/** Tauri command call; rejects with the command's Err value (string or object). */
export function invoke(cmd, args) {
  return mock ? mock.invoke(cmd, args) : T.core.invoke(cmd, args);
}

/** Subscribe to a backend event; resolves to an unlisten function. */
export function listen(event, cb) {
  return mock ? mock.listen(event, cb) : T.event.listen(event, cb);
}

/** Show the (initially hidden) main window. */
export async function showWindow() {
  if (!mock) await T.window.getCurrentWindow().show();
}

/**
 * Native folder picker (QFileDialog.getExistingDirectory). Resolves to the picked path
 * or null when cancelled. The mock answers from `window.__blinkerMockPicks` (see mock.js).
 */
export async function pickFolder(title, defaultPath = "") {
  if (mock) return mock.pick("folder", { title, defaultPath });
  const r = await T.dialog.open({ directory: true, multiple: false, title, defaultPath: defaultPath || undefined });
  return typeof r === "string" && r ? r : null;
}

/**
 * Native open-file picker (QFileDialog.getOpenFileName). `filters` =
 * [{ name, extensions: ["blend"] }, ...]; "*" matches every file.
 */
export async function pickFile(title, filters = [], defaultPath = "") {
  if (mock) return mock.pick("file", { title, filters, defaultPath });
  const r = await T.dialog.open({ directory: false, multiple: false, title, filters, defaultPath: defaultPath || undefined });
  return typeof r === "string" && r ? r : null;
}

/**
 * Native save-file picker (QFileDialog.getSaveFileName); `defaultPath` may be a bare
 * file name. Resolves to the chosen path or null.
 */
export async function pickSaveFile(title, filters = [], defaultPath = "") {
  if (mock) return mock.pick("save", { title, filters, defaultPath });
  const r = await T.dialog.save({ title, filters, defaultPath: defaultPath || undefined });
  return typeof r === "string" && r ? r : null;
}

/** True in debug builds (devtools and reload keys stay available). */
export async function isDev() {
  try {
    return !!(await invoke("is_dev"));
  } catch {
    return false;
  }
}
