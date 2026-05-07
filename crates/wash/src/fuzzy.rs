//! Whitespace + Unicode normalization for *matching only*. The user's `newText` is written
//! verbatim; we only normalize when locating where to splice.

/// Returns the normalized form of `s` for matching.
pub fn normalize_for_match(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match unicode_remap(ch) {
            Some("") => continue,
            Some(replacement) => out.push_str(replacement),
            None => out.push(ch),
        }
    }
    // Collapse runs of [ \t]+ to a single space.
    let mut collapsed = String::with_capacity(out.len());
    let mut last_was_space = false;
    for ch in out.chars() {
        if ch == ' ' || ch == '\t' {
            if !last_was_space {
                collapsed.push(' ');
                last_was_space = true;
            }
            continue;
        }
        last_was_space = false;
        collapsed.push(ch);
    }
    // Trim trailing whitespace on each line.
    collapsed
        .split('\n')
        .map(|l| l.trim_end_matches([' ', '\t']))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Find `needle` inside `haystack`. Returns ranges `[start, end)` on the *original* haystack
/// byte indexes. Normalization is performed on a parallel string with a back-map so the
/// splice happens against the original text.
pub fn fuzzy_find_all(haystack: &str, needle: &str) -> Vec<(usize, usize)> {
    let (normalized, map_back) = normalize_with_map(haystack);
    let norm_needle = normalize_for_match(needle);
    if norm_needle.is_empty() {
        return Vec::new();
    }
    let mut matches = Vec::new();
    let mut from = 0usize;
    while let Some(rel) = normalized[from..].find(&norm_needle) {
        let idx = from + rel;
        let start = map_back.get(idx).copied().unwrap_or(haystack.len());
        let end_norm_idx = idx + norm_needle.len();
        let end = if end_norm_idx >= map_back.len() {
            haystack.len()
        } else {
            map_back[end_norm_idx]
        };
        matches.push((start, end));
        from = idx + norm_needle.len().max(1);
        if from > normalized.len() {
            break;
        }
    }
    matches
}

/// Build the normalized string alongside a map from each *byte* position in `normalized`
/// back to the original byte position in `s`.
fn normalize_with_map(s: &str) -> (String, Vec<usize>) {
    let mut normalized = String::with_capacity(s.len());
    let mut map_back: Vec<usize> = Vec::with_capacity(s.len());
    let mut last_was_space = false;
    for (byte_off, raw) in s.char_indices() {
        let ch_str: String = match unicode_remap(raw) {
            Some("") => continue,
            Some(rep) => rep.to_string(),
            None => raw.to_string(),
        };
        for ch in ch_str.chars() {
            if ch == ' ' || ch == '\t' {
                if last_was_space {
                    continue;
                }
                push_with_map(&mut normalized, &mut map_back, byte_off, ' ');
                last_was_space = true;
                continue;
            }
            if ch == '\n' {
                while normalized.ends_with(' ') {
                    normalized.pop();
                    map_back.pop();
                }
                push_with_map(&mut normalized, &mut map_back, byte_off, '\n');
                last_was_space = false;
                continue;
            }
            push_with_map(&mut normalized, &mut map_back, byte_off, ch);
            last_was_space = false;
        }
    }
    (normalized, map_back)
}

fn push_with_map(out: &mut String, map: &mut Vec<usize>, src_byte: usize, ch: char) {
    let mut buf = [0u8; 4];
    let s = ch.encode_utf8(&mut buf);
    out.push_str(s);
    for _ in 0..s.len() {
        map.push(src_byte);
    }
}

fn unicode_remap(ch: char) -> Option<&'static str> {
    Some(match ch {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' => "'",
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' => "\"",
        '\u{2013}' | '\u{2014}' | '\u{2212}' => "-",
        '\u{00A0}' | '\u{202F}' | '\u{2009}' => " ",
        '\u{200B}' => "",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_runs_of_whitespace() {
        assert_eq!(normalize_for_match("a  \t  b"), "a b");
    }

    #[test]
    fn smart_quotes_match_ascii() {
        let haystack = r#"const x = "hello""#;
        let matches = fuzzy_find_all(haystack, "const x = \u{201C}hello\u{201D}");
        assert_eq!(matches.len(), 1, "expected 1 match, got {matches:?}");
    }

    #[test]
    fn tab_matches_spaces() {
        let haystack = "fn foo() {\n\treturn 1\n}";
        let needle = "fn foo() {\n    return 1\n}";
        let matches = fuzzy_find_all(haystack, needle);
        assert_eq!(matches.len(), 1, "expected 1 match, got {matches:?}");
        let (start, end) = matches[0];
        assert_eq!(start, 0);
        assert_eq!(end, haystack.len());
    }

    #[test]
    fn no_match_returns_empty() {
        let matches = fuzzy_find_all("hello", "world");
        assert!(matches.is_empty());
    }
}
