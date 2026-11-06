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
 * Native "pick a file" dialog.
 * TODO(Task 10): wire to tauri-plugin-dialog (not installed yet); returns null for now,
 * so the "Pick the Blender executable now?" flow ends without launching.
 */
export async function pickFile(_options) {
  return null;
}

/** True in debug builds (devtools and reload keys stay available). */
export async function isDev() {
  try {
    return !!(await invoke("is_dev"));
  } catch {
    return false;
  }
}
