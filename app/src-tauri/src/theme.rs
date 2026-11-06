
use serde_json::Value;

type Map = serde_json::Map<String, Value>;

pub const CUSTOM_PRESET: &str = "Custom";
pub const PRESET_NAMES: [&str; 3] = ["Tokyo Night (default)", "Frigus Nox", "Light"];

pub const COLOR_KEYS: [(&str, &str); 18] = [
    ("window_bg", "Window background"),
    ("panel_bg", "Panel background"),
    ("row_bg", "Row background"),
    ("row_selected_bg", "Selected row"),
    ("row_hover_bg", "Hover row"),
    ("row_border", "Row border"),
    ("row_text_fg", "Row text"),
    ("row_meta_fg", "Row meta"),
    ("accent_fg", "Accent / selected border"),
    ("status_running_fg", "Status running"),
    ("status_stopped_fg", "Status stopped"),
    ("output_bg", "Output background"),
    ("output_fg", "Output text"),
    ("button_bg", "Button"),
    ("button_hover_bg", "Button hover"),
    ("button_text_fg", "Button text"),
    ("input_bg", "Input background"),
    ("input_text_fg", "Input text"),
];

fn build(overrides: &[(&str, &str)]) -> Map {
    let mut m = Map::new();
    m.insert("ui_font_family".into(), Value::from("Segoe UI"));
    m.insert("ui_font_size".into(), Value::from(10));
    m.insert("mono_font_family".into(), Value::from("Cascadia Mono"));
    m.insert("mono_font_size".into(), Value::from(10));
    for (k, v) in overrides {
        m.insert((*k).into(), Value::from(*v));
    }
    m
}

fn colors(values: [&str; 18]) -> Map {
    let pairs: Vec<(&str, &str)> = COLOR_KEYS.iter().map(|(k, _)| *k).zip(values).collect();
    build(&pairs)
}

pub fn preset(name: &str) -> Option<Map> {
    match name {
        "Tokyo Night (default)" => Some(colors([
            "#1e1f22", "#2b2d30", "#2b2d30", "#3b3e44", "#34373c", "#3b3e44", "#e8eaed",
            "#9aa0a6", "#7aa2f7", "#9ece6a", "#7d8590", "#16181d", "#d4d4d4", "#3b3e44",
            "#4a4e55", "#e8eaed", "#2b2d30", "#e8eaed",
        ])),
        "Frigus Nox" => Some(colors([
            "#21252b", "#2c3037", "#2c3037", "#3a4f6b", "#363b44", "#363b44", "#e7e9eb",
            "#858585", "#6d95c0", "#79c3ab", "#587584", "#21252b", "#c6c6c6", "#2c3037",
            "#363b44", "#e7e9eb", "#21252b", "#e7e9eb",
        ])),
        "Light" => Some(colors([
            "#fafbfc", "#eef0f3", "#ffffff", "#e3edff", "#f3f5f8", "#d0d4d9", "#1f2328",
            "#656d76", "#0969da", "#1a7f37", "#6e7681", "#f6f8fa", "#1f2328", "#ffffff",
            "#eef0f3", "#1f2328", "#ffffff", "#1f2328",
        ])),
        _ => None,
    }
}

/// Default theme with every key present in `user` overriding it.
pub fn merge_with_default(user: &Map) -> Map {
    let mut m = preset(PRESET_NAMES[0]).expect("default preset exists");
    for (k, v) in user {
        m.insert(k.clone(), v.clone());
    }
    m
}

/// Name of the built-in preset whose 18 colours all equal `theme`'s, else "Custom".
pub fn detect_preset(theme: &Map) -> String {
    for name in PRESET_NAMES {
        let p = preset(name).expect("builtin preset exists");
        if COLOR_KEYS.iter().all(|(k, _)| theme.get(*k) == p.get(*k)) {
            return name.to_string();
        }
    }
    CUSTOM_PRESET.to_string()
}

pub fn normalize_hex(s: &str) -> Option<String> {
    let s = s.trim().trim_start_matches('#');
    let is_hex = |s: &str| s.chars().all(|c| c.is_ascii_hexdigit());
    let expanded: String = if s.chars().count() == 3 && is_hex(s) {
        s.chars().flat_map(|c| [c, c]).collect()
    } else {
        s.to_string()
    };
    if expanded.len() == 6 && is_hex(&expanded) {
        Some(format!("#{}", expanded.to_lowercase()))
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// export / import
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct ImportedTheme {
    pub theme: Map,
    pub unknown: Vec<String>,
    pub invalid: Vec<String>,
}

const FONT_SIZE_KEYS: [&str; 2] = ["ui_font_size", "mono_font_size"];
const FONT_FAMILY_KEYS: [&str; 2] = ["ui_font_family", "mono_font_family"];

/// DEFAULT_THEME key order: the four font keys, then COLOR_KEYS.
fn default_key_order() -> impl Iterator<Item = &'static str> {
    ["ui_font_family", "ui_font_size", "mono_font_family", "mono_font_size"]
        .into_iter()
        .chain(COLOR_KEYS.iter().map(|(k, _)| *k))
}

fn is_theme_key(k: &str) -> bool {
    default_key_order().any(|d| d == k)
}

/// `json.dumps(theme, indent=2)` (ensure_ascii) with Python's dict order for the
/// dialog's draft theme: DEFAULT_THEME keys first, then any others (sorted here), and
/// the line ends `Path.write_text` produces (CRLF on Windows).
pub fn theme_to_json(theme: &Map) -> String {
    // serde_json's Map is sorted, so the text is built entry by entry.
    let mut entries: Vec<(&str, &Value)> = default_key_order().filter_map(|k| theme.get(k).map(|v| (k, v))).collect();
    entries.extend(theme.iter().filter(|(k, _)| !is_theme_key(k)).map(|(k, v)| (k.as_str(), v)));
    let nl = if cfg!(windows) { "\r\n" } else { "\n" };
    if entries.is_empty() {
        return "{}".into();
    }
    let mut out = String::from("{");
    for (i, (k, v)) in entries.iter().enumerate() {
        out.push_str(if i == 0 { "\n" } else { ",\n" });
        let val = serde_json::to_string_pretty(v).unwrap_or_else(|_| "null".into()).replace('\n', "\n  ");
        out.push_str(&format!("  {}: {}", serde_json::to_string(k).unwrap_or_default(), val));
    }
    out.push_str("\n}");
    ascii_escape(&out).replace('\n', nl)
}

/// Python's `ensure_ascii`: every non-ASCII char as `\uXXXX` (UTF-16 surrogates).
/// Non-ASCII only occurs inside JSON strings, so a global pass is safe.
fn ascii_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii() {
            out.push(c);
        } else {
            let mut buf = [0u16; 2];
            for u in c.encode_utf16(&mut buf) {
                out.push_str(&format!("\\u{u:04x}"));
            }
        }
    }
    out
}

/// `_export_theme`: `Path(path).write_text(json.dumps(draft, indent=2), encoding="utf-8")`.
pub fn write_theme_file(path: &std::path::Path, theme: &Map) -> std::io::Result<()> {
    std::fs::write(path, theme_to_json(theme))
}

/// A JSON object's entries in file order (serde_json's Map is sorted).
struct Entries(Vec<(String, Value)>);

impl<'de> serde::Deserialize<'de> for Entries {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Entries;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: serde::de::MapAccess<'de>>(self, mut m: A) -> Result<Entries, A::Error> {
                let mut v: Vec<(String, Value)> = Vec::new();
                while let Some((k, val)) = m.next_entry::<String, Value>()? {
                    // Python dict: a repeated key keeps its first position, last value.
                    match v.iter_mut().find(|(e, _)| *e == k) {
                        Some(e) => e.1 = val,
                        None => v.push((k, val)),
                    }
                }
                Ok(Entries(v))
            }
        }
        d.deserialize_map(V)
    }
}

/// Python `int(v)` for a JSON value: ints, floats (truncated), bools, and strings of
/// an optionally signed decimal with single `_` between digits, surrounding blanks
/// allowed. Anything else is invalid (TypeError/ValueError).
fn python_int(v: &Value) -> Option<i64> {
    match v {
        Value::Bool(b) => Some(*b as i64),
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f.trunc() as i64)),
        Value::String(s) => {
            let t = s.trim();
            let (neg, digits) = match t.as_bytes().first() {
                Some(b'+') => (false, &t[1..]),
                Some(b'-') => (true, &t[1..]),
                _ => (false, t),
            };
            let ok = !digits.is_empty()
                && digits.split('_').all(|g| !g.is_empty() && g.bytes().all(|b| b.is_ascii_digit()));
            if !ok {
                return None;
            }
            let n: i64 = digits.replace('_', "").parse().ok()?;
            Some(if neg { -n } else { n })
        }
        _ => None,
    }
}

/// Python `repr(float)`: shortest round-trip digits, exponent form outside 1e-4..1e16.
fn python_float(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let sci = format!("{f:e}"); // e.g. "-2.5e300", "1e16", "1.5e0"
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-4..16).contains(&exp) {
        let s = format!("{f}");
        if s.contains('.') { s } else { format!("{s}.0") }
    } else {
        format!("{mant}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

/// Python `repr(str)`: single quotes unless the text has ' and no ".
fn python_repr_str(s: &str) -> String {
    let q = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::from(q);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == q => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(q);
    out
}

fn python_repr(v: &Value) -> String {
    match v {
        Value::String(s) => python_repr_str(s),
        other => python_str(other),
    }
}

/// Python `str(v)` for a value `json.loads` produced.
fn python_str(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => if *b { "True" } else { "False" }.into(),
        Value::Number(n) => match n.as_i64().map(|i| i.to_string()).or_else(|| n.as_u64().map(|u| u.to_string())) {
            Some(s) => s,
            None => python_float(n.as_f64().unwrap_or(f64::NAN)),
        },
        Value::String(s) => s.clone(),
        Value::Array(a) => format!("[{}]", a.iter().map(python_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(o) => format!(
            "{{{}}}",
            o.iter().map(|(k, v)| format!("{}: {}", python_repr_str(k), python_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// `_import_theme` on the file text: a JSON object, else an error with the text the
/// "Import failed" box shows. Keys of DEFAULT_THEME are coerced as Python does (sizes
/// `int(v)`, families `str(v)`, colours `_normalize_hex(str(v))`); the accepted values
/// are returned in `theme`, other keys in `unknown`, failed coercions in `invalid`
/// (both in file order).
pub fn parse_theme_import(text: &str) -> Result<ImportedTheme, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    if !v.is_object() {
        return Err("Theme file must be a JSON object".into());
    }
    let Entries(entries) = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let mut r = ImportedTheme { theme: Map::new(), unknown: Vec::new(), invalid: Vec::new() };
    for (k, v) in entries {
        if !is_theme_key(&k) {
            r.unknown.push(k);
            continue;
        }
        let coerced = if FONT_SIZE_KEYS.contains(&k.as_str()) {
            python_int(&v).map(Value::from)
        } else if FONT_FAMILY_KEYS.contains(&k.as_str()) {
            Some(Value::from(python_str(&v)))
        } else {
            normalize_hex(&python_str(&v)).map(Value::from)
        };
        match coerced {
            Some(c) => {
                r.theme.insert(k, c);
            }
            None => r.invalid.push(k),
        }
    }
    Ok(r)
}

/// `_import_theme`: read the file (UTF-8) and parse it.
pub fn read_theme_file(path: &std::path::Path) -> Result<ImportedTheme, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    parse_theme_import(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Values copied from blinker_ui.py, in COLOR_KEYS order.
    const TOKYO: [&str; 18] = [
        "#1e1f22", "#2b2d30", "#2b2d30", "#3b3e44", "#34373c", "#3b3e44", "#e8eaed", "#9aa0a6",
        "#7aa2f7", "#9ece6a", "#7d8590", "#16181d", "#d4d4d4", "#3b3e44", "#4a4e55", "#e8eaed",
        "#2b2d30", "#e8eaed",
    ];
    const FRIGUS: [&str; 18] = [
        "#21252b", "#2c3037", "#2c3037", "#3a4f6b", "#363b44", "#363b44", "#e7e9eb", "#858585",
        "#6d95c0", "#79c3ab", "#587584", "#21252b", "#c6c6c6", "#2c3037", "#363b44", "#e7e9eb",
        "#21252b", "#e7e9eb",
    ];
    const LIGHT: [&str; 18] = [
        "#fafbfc", "#eef0f3", "#ffffff", "#e3edff", "#f3f5f8", "#d0d4d9", "#1f2328", "#656d76",
        "#0969da", "#1a7f37", "#6e7681", "#f6f8fa", "#1f2328", "#ffffff", "#eef0f3", "#1f2328",
        "#ffffff", "#1f2328",
    ];

    fn s(v: &Map, k: &str) -> String {
        v[k].as_str().unwrap().to_string()
    }

    #[test]
    fn default_has_all_18_color_keys() {
        let t = preset("Tokyo Night (default)").unwrap();
        for (k, _) in COLOR_KEYS {
            assert!(t.contains_key(k), "missing {k}");
        }
        assert_eq!(COLOR_KEYS.len(), 18);
        assert_eq!(COLOR_KEYS[0].0, "window_bg");
        assert_eq!(COLOR_KEYS[17], ("input_text_fg", "Input text"));
    }

    #[test]
    fn all_presets_match_python_values() {
        for (name, expected) in [
            ("Tokyo Night (default)", TOKYO),
            ("Frigus Nox", FRIGUS),
            ("Light", LIGHT),
        ] {
            let p = preset(name).unwrap();
            for (i, (k, _)) in COLOR_KEYS.iter().enumerate() {
                assert_eq!(s(&p, k), expected[i], "{name}.{k}");
            }
            assert_eq!(s(&p, "ui_font_family"), "Segoe UI", "{name}");
            assert_eq!(p["ui_font_size"], 10, "{name}");
            assert_eq!(s(&p, "mono_font_family"), "Cascadia Mono", "{name}");
            assert_eq!(p["mono_font_size"], 10, "{name}");
            assert_eq!(p.len(), 22, "{name}");
        }
        assert!(preset("nope").is_none());
    }

    #[test]
    fn frigus_nox_window_bg_is_21252b() {
        assert_eq!(s(&preset("Frigus Nox").unwrap(), "window_bg"), "#21252b");
    }

    #[test]
    fn light_accent_is_0969da() {
        assert_eq!(s(&preset("Light").unwrap(), "accent_fg"), "#0969da");
    }

    #[test]
    fn normalize_hex_accepts_3_and_6_digits_with_or_without_hash() {
        assert_eq!(normalize_hex("#ABC").as_deref(), Some("#aabbcc"));
        assert_eq!(normalize_hex("abc").as_deref(), Some("#aabbcc"));
        assert_eq!(normalize_hex("  #1E1F22 ").as_deref(), Some("#1e1f22"));
        assert_eq!(normalize_hex("1e1f22").as_deref(), Some("#1e1f22"));
        assert_eq!(normalize_hex("##abc").as_deref(), Some("#aabbcc"));
        assert_eq!(normalize_hex("abcd"), None);
        assert_eq!(normalize_hex("ggg"), None);
        assert_eq!(normalize_hex(""), None);
        assert_eq!(normalize_hex("#12345"), None);
        assert_eq!(normalize_hex("#1234567"), None);
    }

    #[test]
    fn detect_preset_returns_builtin_names() {
        for name in PRESET_NAMES {
            assert_eq!(detect_preset(&preset(name).unwrap()), name);
        }
    }

    #[test]
    fn detect_preset_returns_custom_for_modified_theme() {
        let mut t = preset("Light").unwrap();
        t.insert("accent_fg".into(), Value::String("#000000".into()));
        assert_eq!(detect_preset(&t), "Custom");
        assert_eq!(detect_preset(&Map::new()), "Custom");
    }

    #[test]
    fn detect_preset_ignores_non_color_keys() {
        let mut t = preset("Frigus Nox").unwrap();
        t.insert("ui_font_size".into(), Value::from(14));
        assert_eq!(detect_preset(&t), "Frigus Nox");
    }

    // ----- export / import (SettingsDialog._export_theme / _import_theme) -----

    use serde_json::json;

    fn obj(v: Value) -> Map {
        v.as_object().unwrap().clone()
    }

    #[test]
    fn export_json_is_python_dumps_indent_2_in_default_key_order() {
        let mut t = preset("Light").unwrap();
        t.insert("zz_extra".into(), json!("x"));
        let s = theme_to_json(&t);
        let nl = if cfg!(windows) { "\r\n" } else { "\n" };
        assert!(s.starts_with(&format!("{{{nl}  \"ui_font_family\": \"Segoe UI\",{nl}  \"ui_font_size\": 10,{nl}")), "{s}");
        assert!(s.contains(&format!("  \"window_bg\": \"#fafbfc\",{nl}")));
        assert!(s.ends_with(&format!("  \"input_text_fg\": \"#1f2328\",{nl}  \"zz_extra\": \"x\"{nl}}}")), "{s}");
        // keys in DEFAULT_THEME order: fonts first, then COLOR_KEYS
        let pos = |k: &str| s.find(&format!("\"{k}\"")).unwrap();
        assert!(pos("mono_font_size") < pos("window_bg"));
        assert!(pos("button_text_fg") < pos("input_bg"));
        // ensure_ascii=True
        let mut u = Map::new();
        u.insert("ui_font_family".into(), json!("Méiryo 😀"));
        let escaped = "\"M\\u00e9iryo \\ud83d\\ude00\"";
        assert!(theme_to_json(&u).contains(escaped), "{}", theme_to_json(&u));
    }

    #[test]
    fn export_import_round_trip_through_a_file() {
        let t = tempfile::tempdir().unwrap();
        let p = t.path().join("blinker_theme.json");
        let mut theme = preset("Frigus Nox").unwrap();
        theme.insert("ui_font_size".into(), json!(12));
        theme.insert("mono_font_family".into(), json!("Consolas"));
        write_theme_file(&p, &theme).unwrap();
        let back = read_theme_file(&p).unwrap();
        assert_eq!(back.theme, theme);
        assert!(back.unknown.is_empty() && back.invalid.is_empty());
        assert_eq!(detect_preset(&back.theme), "Frigus Nox");
    }

    #[test]
    fn export_to_unwritable_path_is_an_error() {
        let t = tempfile::tempdir().unwrap();
        // a directory in place of the file
        assert!(write_theme_file(t.path(), &preset("Light").unwrap()).is_err());
    }

    #[test]
    fn import_ignores_unknown_keys_in_file_order() {
        let r = parse_theme_import(r##"{"zeta": 1, "accent_fg": "#123456", "alpha": "x", "launchers": []}"##).unwrap();
        assert_eq!(r.unknown, ["zeta", "alpha", "launchers"]);
        assert!(r.invalid.is_empty());
        assert_eq!(r.theme, obj(json!({"accent_fg": "#123456"})));
    }

    #[test]
    fn import_rejects_bad_json_and_non_objects() {
        assert!(parse_theme_import("{not json").is_err());
        assert!(parse_theme_import("").is_err());
        for s in ["[]", "[1, 2]", "\"x\"", "5", "null", "true"] {
            assert_eq!(parse_theme_import(s).unwrap_err(), "Theme file must be a JSON object", "{s}");
        }
    }

    #[test]
    fn import_reports_missing_file() {
        let t = tempfile::tempdir().unwrap();
        assert!(read_theme_file(&t.path().join("nope.json")).is_err());
    }

    #[test]
    fn import_normalises_hex_values_like_python() {
        let r = parse_theme_import(
            r###"{"window_bg": " #ABC ", "panel_bg": "1E1F22", "row_bg": 123, "row_border": 123456,
                "accent_fg": "#12345", "output_bg": null, "output_fg": true, "button_bg": ["#fff"],
                "input_bg": "##aabbcc"}"###,
        )
        .unwrap();
        assert_eq!(
            r.theme,
            obj(json!({
                "window_bg": "#aabbcc", "panel_bg": "#1e1f22", "row_bg": "#112233",
                "row_border": "#123456", "input_bg": "#aabbcc"
            }))
        );
        assert_eq!(r.invalid, ["accent_fg", "output_bg", "output_fg", "button_bg"]);
    }

    #[test]
    fn import_font_sizes_use_python_int() {
        let r = parse_theme_import(
            r#"{"ui_font_size": "12", "mono_font_size": 9.9}"#,
        )
        .unwrap();
        assert_eq!(r.theme, obj(json!({"ui_font_size": 12, "mono_font_size": 9})));
        let r = parse_theme_import(r#"{"ui_font_size": " +1_4 ", "mono_font_size": true}"#).unwrap();
        assert_eq!(r.theme, obj(json!({"ui_font_size": 14, "mono_font_size": 1})));
        for bad in [r#""12.5""#, r#""abc""#, "null", "[12]", "{}", r#""""#, r#""1__2""#, r#""_12""#] {
            let r = parse_theme_import(&format!(r#"{{"ui_font_size": {bad}}}"#)).unwrap();
            assert_eq!(r.invalid, ["ui_font_size"], "{bad}");
            assert!(r.theme.is_empty(), "{bad}");
        }
    }

    #[test]
    fn import_font_families_use_python_str() {
        let r = parse_theme_import(
            r#"{"ui_font_family": "Consolas", "mono_font_family": 10.0}"#,
        )
        .unwrap();
        assert_eq!(r.theme, obj(json!({"ui_font_family": "Consolas", "mono_font_family": "10.0"})));
        let cases = [
            ("5", "5"), ("null", "None"), ("true", "True"), ("false", "False"), ("1.5", "1.5"),
            ("1e16", "1e+16"), ("1e-5", "1e-05"), ("0.0001", "0.0001"), ("-2.5e300", "-2.5e+300"),
            (r#"[1, "a", null]"#, "[1, 'a', None]"), (r#"{"k": "it's"}"#, r#"{'k': "it's"}"#),
        ];
        for (src, want) in cases {
            let r = parse_theme_import(&format!(r#"{{"ui_font_family": {src}}}"#)).unwrap();
            assert_eq!(r.theme["ui_font_family"], json!(want), "{src}");
            assert!(r.invalid.is_empty());
        }
    }

    #[test]
    fn import_duplicate_keys_keep_first_position_last_value() {
        let r = parse_theme_import(r##"{"x": 1, "accent_fg": "#000000", "y": 2, "x": 3, "accent_fg": "#ffffff"}"##).unwrap();
        assert_eq!(r.unknown, ["x", "y"]);
        assert_eq!(r.theme["accent_fg"], json!("#ffffff"));
    }

    #[test]
    fn merge_fills_missing_keys_from_default() {
        let mut u = Map::new();
        u.insert("accent_fg".into(), Value::String("#123456".into()));
        let m = merge_with_default(&u);
        assert_eq!(s(&m, "accent_fg"), "#123456");
        assert_eq!(s(&m, "window_bg"), "#1e1f22");
        assert_eq!(m["ui_font_size"], 10);
        assert_eq!(m.len(), 22);
    }
}
