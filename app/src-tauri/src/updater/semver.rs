/// `_parse_semver`: strip, drop leading `v`/`V`, cut at `-` then `+`, three dot parts
/// (missing ones 0), each `int()` or 0 when it is not a number.
pub fn parse_semver(s: &str) -> (u32, u32, u32) {
    let s = s.trim().trim_start_matches(['v', 'V']);
    let s = s.split('-').next().unwrap_or("");
    let s = s.split('+').next().unwrap_or("");
    let mut parts = s.split('.').map(py_int).chain(std::iter::repeat(0));
    let mut next = || parts.next().unwrap_or(0);
    (next(), next(), next())
}

/// `int(p)` for a version part, 0 on ValueError. Accepts surrounding whitespace and
/// single underscores between digits; ASCII digits only; saturates at u32::MAX.
fn py_int(p: &str) -> u32 {
    let t = p.trim();
    let ok = !t.is_empty()
        && t.split('_').all(|g| !g.is_empty() && g.bytes().all(|b| b.is_ascii_digit()));
    if !ok {
        return 0;
    }
    let digits: String = t.chars().filter(|&c| c != '_').collect();
    digits.parse::<u32>().unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_orders_0_1_2_below_0_2_0() {
        assert!(parse_semver("0.1.2") < parse_semver("0.2.0"));
        assert!(parse_semver("v0.1.10") > parse_semver("v0.1.9"));
        assert!(parse_semver("v0.1.0") <= parse_semver("0.1.0"));
    }

    #[test]
    fn semver_ignores_prefix_and_suffix() {
        assert_eq!(parse_semver("v1.2.3"), (1, 2, 3));
        assert_eq!(parse_semver("V1.2.3-beta.1"), (1, 2, 3));
        assert_eq!(parse_semver("1.2.3+build.7"), (1, 2, 3));
        assert_eq!(parse_semver("  vv2.0-rc1+x "), (2, 0, 0));
        assert_eq!(parse_semver("v0.2"), (0, 2, 0));
        assert_eq!(parse_semver(""), (0, 0, 0));
        assert_eq!(parse_semver("v1.x.3"), (1, 0, 3));
        assert_eq!(parse_semver("1.2.3.4"), (1, 2, 3));
        // int(): surrounding whitespace and digit-separating underscores are accepted
        assert_eq!(parse_semver("1. 2 .1_0"), (1, 2, 10));
        assert_eq!(parse_semver("1._2.3_"), (1, 0, 0));
        assert_eq!(parse_semver("99999999999.1.1"), (u32::MAX, 1, 1));
    }
}
