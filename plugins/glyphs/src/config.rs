//! `glyphs.txt`: which title textures hold which button glyphs.
//!
//! ```text
//! # <hash> <glyph> [x y w h] [menu] [premul]
//! 3fa2c9d1e0b4a785 A                      the whole texture is the A button
//! 9c01d2e3f4a5b6c7 A  0  0 64 64          an atlas: one glyph a line
//! 9c01d2e3f4a5b6c7 B 64  0 64 64 menu     ... with the key B stands for in menus
//! 9c01d2e3f4a5b6c7 key=Tab 128 0 64 64    a key of your choosing
//! 9c01d2e3f4a5b6c7 label=HOLD 192 0 64 64 a cap that says HOLD
//! map RB E                                RB's caps show E, whatever the layout says
//! map A Enter menu                        ... in menus
//! ```
//!
//! The hash is the first 16 hex digits of a dump file's name, which may be
//! pasted whole (`3fa2c9d1e0b4a785_64x64_BC3.png`). Rectangles are in texels
//! of the dumped image.

/// One line naming a texture.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub hash: u64,
    pub what: What,
    /// `[x, y, w, h]`; the whole texture if `None`.
    pub rect: Option<[u32; 4]>,
    pub menu: bool,
    pub premultiplied: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum What {
    Glyph(String),
    Key(String),
    Label(String),
}

/// A `map` line.
#[derive(Debug, Clone, PartialEq)]
pub struct Map {
    pub glyph: &'static str,
    pub key: String,
    pub menu: bool,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    pub entries: Vec<Entry>,
    pub maps: Vec<Map>,
    /// Lines not understood: (line number, why).
    pub errors: Vec<(usize, String)>,
}

/// The hash at the start of `token`: 16 hex digits, maybe after `0x`.
pub fn parse_hash(token: &str) -> Option<u64> {
    let t = token.strip_prefix("0x").unwrap_or(token);
    let digits = t.get(..16)?;
    if !t[16..].is_empty() && !t[16..].starts_with(['_', '.', '-']) {
        return None;
    }
    u64::from_str_radix(digits, 16).ok()
}

pub fn parse(text: &str) -> Config {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut config = Config::default();
    for (n, line) in text.lines().enumerate() {
        let n = n + 1;
        let line = line.split('#').next().unwrap_or("");
        let tokens: Vec<&str> = line.split_whitespace().collect();
        if tokens.is_empty() {
            continue;
        }
        if tokens[0].eq_ignore_ascii_case("map") {
            match tokens.as_slice() {
                [_, glyph, key, rest @ ..] => {
                    let Some(glyph) = crate::keys::canonical_glyph(glyph) else {
                        config.errors.push((n, format!("no glyph {glyph}")));
                        continue;
                    };
                    if !crate::keys::known_key(key) {
                        config.errors.push((n, format!("no key {key}")));
                        continue;
                    }
                    let menu = rest.iter().any(|t| t.eq_ignore_ascii_case("menu"));
                    config.maps.push(Map { glyph, key: key.to_string(), menu });
                }
                _ => config.errors.push((n, "map <glyph> <key> [menu]".into())),
            }
            continue;
        }
        let Some(hash) = parse_hash(tokens[0]) else {
            config.errors.push((n, format!("{} is not a 16-digit hash", tokens[0])));
            continue;
        };
        let Some(&what) = tokens.get(1) else {
            config.errors.push((n, "a glyph after the hash".into()));
            continue;
        };
        let what = if let Some(key) = what.strip_prefix("key=") {
            if !crate::keys::known_key(key) {
                config.errors.push((n, format!("no key {key}")));
                continue;
            }
            What::Key(key.to_string())
        } else if let Some(label) = what.strip_prefix("label=") {
            What::Label(label.to_ascii_uppercase())
        } else if let Some(g) = crate::keys::canonical_glyph(what) {
            What::Glyph(g.to_string())
        } else {
            config.errors.push((n, format!("no glyph {what}")));
            continue;
        };
        let numbers: Vec<u32> = tokens[2..].iter().filter_map(|t| t.parse().ok()).collect();
        let words: Vec<&str> = tokens[2..].iter().copied().filter(|t| t.parse::<u32>().is_err()).collect();
        let rect = match numbers.as_slice() {
            [] => None,
            [x, y, w, h] if *w > 0 && *h > 0 => Some([*x, *y, *w, *h]),
            _ => {
                config.errors.push((n, "a rectangle is x y w h, w and h above 0".into()));
                continue;
            }
        };
        let mut menu = false;
        let mut premultiplied = false;
        let mut bad = None;
        for w in words {
            match w.to_ascii_lowercase().as_str() {
                "menu" => menu = true,
                "premul" => premultiplied = true,
                _ => bad = Some(w),
            }
        }
        if let Some(w) = bad {
            config.errors.push((n, format!("{w}: not menu or premul")));
            continue;
        }
        config.entries.push(Entry { hash, what, rect, menu, premultiplied });
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_and_file_names() {
        assert_eq!(parse_hash("3fa2c9d1e0b4a785"), Some(0x3fa2c9d1e0b4a785));
        assert_eq!(parse_hash("0x3FA2C9D1E0B4A785"), Some(0x3fa2c9d1e0b4a785));
        assert_eq!(parse_hash("3fa2c9d1e0b4a785_64x64_BC3.png"), Some(0x3fa2c9d1e0b4a785));
        assert_eq!(parse_hash("3fa2c9d1e0b4a78"), None);
        assert_eq!(parse_hash("3fa2c9d1e0b4a7850"), None);
        assert_eq!(parse_hash("zzzzzzzzzzzzzzzz"), None);
    }

    #[test]
    fn lines_of_every_kind() {
        let c = parse(
            "# glyphs\n\
             3fa2c9d1e0b4a785 A\n\
             9c01d2e3f4a5b6c7_256x64_BC7.png b 64 0 64 64 menu premul  # an atlas\n\
             9c01d2e3f4a5b6c7 key=Tab 128 0 64 64\n\
             9c01d2e3f4a5b6c7 label=hold 192 0 64 64\n\
             map RB E\n\
             MAP start Esc menu\n",
        );
        assert!(c.errors.is_empty(), "{:?}", c.errors);
        assert_eq!(c.entries.len(), 4);
        assert_eq!(c.entries[0], Entry { hash: 0x3fa2c9d1e0b4a785, what: What::Glyph("A".into()), rect: None, menu: false, premultiplied: false });
        assert_eq!(c.entries[1].what, What::Glyph("B".into()));
        assert_eq!(c.entries[1].rect, Some([64, 0, 64, 64]));
        assert!(c.entries[1].menu && c.entries[1].premultiplied);
        assert_eq!(c.entries[2].what, What::Key("Tab".into()));
        assert_eq!(c.entries[3].what, What::Label("HOLD".into()));
        assert_eq!(c.maps, vec![
            Map { glyph: "RB", key: "E".into(), menu: false },
            Map { glyph: "MENU", key: "Esc".into(), menu: true },
        ]);
    }

    #[test]
    fn mistakes_are_told_by_line() {
        let c = parse("nothex A\n3fa2c9d1e0b4a785\n3fa2c9d1e0b4a785 Q\n3fa2c9d1e0b4a785 A 1 2 3\n3fa2c9d1e0b4a785 A 0 0 0 4\n3fa2c9d1e0b4a785 A huh\nmap A Banana\n3fa2c9d1e0b4a785 key=F13\n");
        assert!(c.entries.is_empty());
        assert_eq!(c.errors.iter().map(|e| e.0).collect::<Vec<_>>(), [1, 2, 3, 4, 5, 6, 7, 8]);
    }
}
