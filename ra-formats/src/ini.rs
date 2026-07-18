//! A small INI reader matching the semantics of the original engine's
//! `INIClass` (`common/ini.cpp`) closely enough for scenario files:
//!
//! - Sections are `[Name]`; the name is trimmed.
//! - Entries are `key=value`; split on the first `=`, both sides trimmed. Empty
//!   key or empty value is dropped.
//! - `;` starts a comment (to end of line); blank lines are ignored.
//! - Section and key lookups are **case-insensitive** (the original uppercases
//!   names before hashing).
//! - Section and entry order is preserved — essential for reassembling the
//!   numbered `[MapPack]` lines in the right order.
//!
//! This is a pure text parser: no game knowledge lives here (scenario semantics
//! belong in `ra-data`).

use std::collections::BTreeMap;

/// A parsed INI file: an ordered list of sections, each an ordered list of
/// `(key, value)` entries. Lookups are case-insensitive.
#[derive(Debug, Clone, Default)]
pub struct Ini {
    sections: Vec<Section>,
    /// Uppercased section name -> index into `sections`.
    index: BTreeMap<String, usize>,
}

#[derive(Debug, Clone)]
struct Section {
    #[allow(dead_code)]
    name: String,
    /// Entries in file order: (original-case key, value).
    entries: Vec<(String, String)>,
    /// Uppercased key -> index into `entries` (first occurrence wins).
    index: BTreeMap<String, usize>,
}

fn strip_comment(line: &str) -> &str {
    match line.find(';') {
        Some(i) => &line[..i],
        None => line,
    }
}

impl Ini {
    /// Parse INI text.
    pub fn parse(text: &str) -> Ini {
        let mut ini = Ini::default();
        let mut cur: Option<usize> = None;

        for raw in text.lines() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            if let Some(rest) = line.strip_prefix('[') {
                // Section header: text up to the closing bracket.
                let name = match rest.find(']') {
                    Some(i) => rest[..i].trim(),
                    None => rest.trim(),
                };
                let key = name.to_ascii_uppercase();
                if let Some(&existing) = ini.index.get(&key) {
                    cur = Some(existing); // merge duplicate section
                } else {
                    let idx = ini.sections.len();
                    ini.sections.push(Section {
                        name: name.to_string(),
                        entries: Vec::new(),
                        index: BTreeMap::new(),
                    });
                    ini.index.insert(key, idx);
                    cur = Some(idx);
                }
                continue;
            }
            // Entry line.
            let Some(eq) = line.find('=') else { continue };
            let key = line[..eq].trim();
            let value = line[eq + 1..].trim();
            if key.is_empty() || value.is_empty() {
                continue;
            }
            if let Some(si) = cur {
                let sec = &mut ini.sections[si];
                let ukey = key.to_ascii_uppercase();
                if !sec.index.contains_key(&ukey) {
                    sec.index.insert(ukey, sec.entries.len());
                }
                sec.entries.push((key.to_string(), value.to_string()));
            }
        }
        ini
    }

    fn section(&self, name: &str) -> Option<&Section> {
        let key = name.to_ascii_uppercase();
        self.index.get(&key).map(|&i| &self.sections[i])
    }

    /// Whether a section exists (case-insensitive).
    pub fn has_section(&self, name: &str) -> bool {
        self.section(name).is_some()
    }

    /// Get a string value (case-insensitive section and key), trimmed.
    pub fn get(&self, section: &str, key: &str) -> Option<&str> {
        let sec = self.section(section)?;
        let ukey = key.to_ascii_uppercase();
        sec.index.get(&ukey).map(|&i| sec.entries[i].1.as_str())
    }

    /// Get an integer value, honoring the original's decimal / `$hex` / `NNh`
    /// forms (`common/ini.cpp` `Get_Int`). Returns `None` if absent/unparseable.
    pub fn get_int(&self, section: &str, key: &str) -> Option<i64> {
        let v = self.get(section, key)?.trim();
        if let Some(hex) = v.strip_prefix('$') {
            return i64::from_str_radix(hex, 16).ok();
        }
        if let Some(hex) = v.strip_suffix(['h', 'H']) {
            if let Ok(n) = i64::from_str_radix(hex, 16) {
                return Some(n);
            }
        }
        // atoi-like: take a leading optional sign and digits.
        let bytes = v.as_bytes();
        let mut i = 0;
        let mut sign = 1i64;
        if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
            if bytes[i] == b'-' {
                sign = -1;
            }
            i += 1;
        }
        let start = i;
        let mut acc: i64 = 0;
        while i < bytes.len() && bytes[i].is_ascii_digit() {
            acc = acc.checked_mul(10)?.checked_add((bytes[i] - b'0') as i64)?;
            i += 1;
        }
        if i == start {
            return None;
        }
        Some(sign * acc)
    }

    /// The entries of a section in file order, as `(key, value)` pairs. Used to
    /// reassemble numbered blocks like `[MapPack]`.
    pub fn section_entries(&self, section: &str) -> Option<&[(String, String)]> {
        self.section(section).map(|s| s.entries.as_slice())
    }

    /// Concatenate the values of a numbered block section (`1=`, `2=`, …) in
    /// file order — the exact input the base64 decoder expects for `[MapPack]`
    /// and friends.
    pub fn concat_block(&self, section: &str) -> Option<String> {
        let sec = self.section(section)?;
        let mut s = String::new();
        for (_, v) in &sec.entries {
            s.push_str(v);
        }
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sections_and_entries() {
        let ini = Ini::parse("[Map]\nTheater=SNOW\nX=49 ; a comment\nWidth=30\n");
        assert!(ini.has_section("map")); // case-insensitive
        assert_eq!(ini.get("Map", "theater"), Some("SNOW"));
        assert_eq!(ini.get_int("Map", "X"), Some(49)); // comment stripped
        assert_eq!(ini.get_int("Map", "Width"), Some(30));
        assert_eq!(ini.get("Map", "missing"), None);
    }

    #[test]
    fn preserves_block_order() {
        let ini = Ini::parse("[MapPack]\n1=AA\n2=BB\n3=CC\n");
        assert_eq!(ini.concat_block("MapPack").unwrap(), "AABBCC");
        let entries = ini.section_entries("MapPack").unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0], ("1".to_string(), "AA".to_string()));
    }

    #[test]
    fn hex_ints() {
        let ini = Ini::parse("[S]\na=$1F\nb=20h\nc=-5\n");
        assert_eq!(ini.get_int("S", "a"), Some(31));
        assert_eq!(ini.get_int("S", "b"), Some(32));
        assert_eq!(ini.get_int("S", "c"), Some(-5));
    }

    #[test]
    fn duplicate_section_merges() {
        let ini = Ini::parse("[A]\nx=1\n[B]\ny=2\n[A]\nz=3\n");
        assert_eq!(ini.get("A", "x"), Some("1"));
        assert_eq!(ini.get("A", "z"), Some("3"));
    }

    #[test]
    fn duplicate_key_first_occurrence_wins() {
        // The original's INIClass hash-indexes by key, so the first entry for
        // a duplicated key is authoritative; later duplicates are kept in the
        // entry list (order-preserving) but do not shadow it.
        let ini = Ini::parse("[A]\nx=1\nx=2\nx=3\n");
        assert_eq!(ini.get("A", "x"), Some("1"));
        let entries = ini.section_entries("A").unwrap();
        assert_eq!(entries.len(), 3); // all three lines kept in file order
    }

    #[test]
    fn malformed_hex_int_is_none() {
        // `$` prefix with non-hex-digit content should not silently parse.
        let ini = Ini::parse("[S]\na=$ZZ\nb=xxh\n");
        assert_eq!(ini.get_int("S", "a"), None);
        assert_eq!(ini.get_int("S", "b"), None);
    }

    #[test]
    fn empty_hex_prefix_is_none() {
        let ini = Ini::parse("[S]\na=$\nb=h\n");
        assert_eq!(ini.get_int("S", "a"), None);
        // "h" alone: strip_suffix leaves "" which is not valid hex -> falls
        // through to the atoi path, which also finds no digits.
        assert_eq!(ini.get_int("S", "b"), None);
    }

    #[test]
    fn get_int_non_numeric_is_none() {
        let ini = Ini::parse("[S]\na=hello\nb=\n");
        assert_eq!(ini.get_int("S", "a"), None);
        // "b=" has an empty value, so the entry is dropped entirely by the
        // parser (empty key/value lines are skipped) -> key absent.
        assert_eq!(ini.get("S", "b"), None);
    }

    #[test]
    fn get_int_leading_sign_and_trailing_garbage() {
        // atoi-like: a leading sign plus digits is consumed; trailing
        // non-digit garbage after at least one digit still yields the
        // leading numeric value (matches the original's `atoi` semantics).
        let ini = Ini::parse("[S]\na=+42\nb=-7\nc=12abc\n");
        assert_eq!(ini.get_int("S", "a"), Some(42));
        assert_eq!(ini.get_int("S", "b"), Some(-7));
        assert_eq!(ini.get_int("S", "c"), Some(12));
    }

    #[test]
    fn unclosed_section_header_uses_rest_of_line() {
        // No closing ']': the original still opens a section named by
        // whatever follows '[' on the line.
        let ini = Ini::parse("[Unclosed\nx=1\n");
        assert!(ini.has_section("Unclosed"));
        assert_eq!(ini.get("Unclosed", "x"), Some("1"));
    }

    #[test]
    fn entries_before_any_section_are_dropped() {
        let ini = Ini::parse("x=1\n[A]\ny=2\n");
        assert_eq!(ini.get("A", "y"), Some("2"));
        // No section owns the orphan entry; nothing crashes, it's just gone.
        assert!(!ini.has_section(""));
    }

    #[test]
    fn line_without_equals_is_ignored() {
        let ini = Ini::parse("[A]\nnotanentry\nx=1\n");
        assert_eq!(ini.get("A", "x"), Some("1"));
    }

    #[test]
    fn comment_only_line_and_blank_lines_are_skipped() {
        let ini = Ini::parse("[A]\n\n; just a comment\n   \nx=1\n");
        assert_eq!(ini.get("A", "x"), Some("1"));
    }

    #[test]
    fn missing_section_get_is_none() {
        let ini = Ini::parse("[A]\nx=1\n");
        assert_eq!(ini.get("NoSuchSection", "x"), None);
        assert_eq!(ini.get_int("NoSuchSection", "x"), None);
        assert!(ini.section_entries("NoSuchSection").is_none());
        assert!(ini.concat_block("NoSuchSection").is_none());
    }

    #[test]
    fn hex_int_overflow_does_not_panic() {
        // Wildly out-of-range hex should fail to parse rather than overflow.
        let ini = Ini::parse("[S]\na=$FFFFFFFFFFFFFFFFFFFFFFFF\n");
        assert_eq!(ini.get_int("S", "a"), None);
    }

    #[test]
    fn decimal_overflow_does_not_panic() {
        // A decimal literal far beyond i64::MAX must not panic (checked_mul
        // / checked_add return None, propagated as None from get_int).
        let ini = Ini::parse("[S]\na=999999999999999999999999999999\n");
        assert_eq!(ini.get_int("S", "a"), None);
    }
}
