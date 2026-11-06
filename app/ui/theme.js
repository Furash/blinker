// Theme -> CSS custom properties named exactly like the theme keys (--window_bg, ...).
// Font sizes stay unitless (Qt points); app.css turns them into round(pt * 4 / 3) px, as
// Qt does (10pt -> 13px, not 13.33px), scaled by the webview's device pixel ratio.

const FONT_KEYS = ["ui_font_family", "mono_font_family"];

function cssFontFamily(name) {
  return `"${String(name).replace(/["\\]/g, "")}"`;
}

/** Apply a theme dict (already merged with the default) to the document. */
export function applyTheme(theme) {
  const s = document.documentElement.style;
  for (const [k, v] of Object.entries(theme || {})) {
    if (v === null || typeof v === "object") continue;
    if (FONT_KEYS.includes(k)) s.setProperty(`--${k}`, cssFontFamily(v));
    else s.setProperty(`--${k}`, String(v));
  }
  // Qt picks the "<family> Semibold" face for weight 600; Chromium's family lookup
  // only offers regular/bold for "Segoe UI" and would render 600 as bold.
  if (theme && theme.ui_font_family) {
    s.setProperty("--ui_font_semibold", `${cssFontFamily(`${theme.ui_font_family} Semibold`)}, ${cssFontFamily(theme.ui_font_family)}`);
  }
}

/** `{**default, **theme}`. */
export function mergeTheme(defaults, theme) {
  return { ...(defaults || {}), ...(theme || {}) };
}
