#![allow(dead_code)]

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
