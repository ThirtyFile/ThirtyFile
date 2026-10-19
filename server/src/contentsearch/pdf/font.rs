//! Turning the bytes of a text string into characters: a font says what each code stands for, by its `ToUnicode` map
//! (any font made by a program that cared about copying text), else by its encoding (the named one, with the changes a
//! font lists by glyph name), else, for a font of two-byte codes whose encoding is a Unicode one, by the codes themselves.

use std::collections::HashMap;

use super::object::{Dict, Doc, Lexer, Obj, Tok};

/// Entries a code map may hold at most
const MAX_MAP: usize = 200_000;

/// The characters of 0x80 to 0x9F in Windows-1252
const CP1252_HIGH: &str = "€\u{81}‚ƒ„…†‡ˆ‰Š‹Œ\u{8D}Ž\u{8F}\u{90}‘’“”•–—˜™š›œ\u{9D}žŸ";
/// The characters of 0x80 to 0xFF in Mac OS Roman
const MAC_ROMAN: &str = "ÄÅÇÉÑÖÜáàâäãåçéèêëíìîïñóòôöõúùûü†°¢£§•¶ß®©™´¨≠ÆØ∞±≤≥¥µ∂∑∏π∫ªºΩæø¿¡¬√ƒ≈∆«»… ÀÃÕŒœ–—“”‘’÷◊ÿŸ⁄€‹›ﬁﬂ‡·‚„‰ÂÊÁËÈÍÎÏÌÓÔ ÒÚÛÙıˆ˜¯˘˙˚¸˝˛ˇ";
/// The glyph names of 0xA0 to 0xFF in Latin-1 order
const LATIN1_NAMES: &str = "space exclamdown cent sterling currency yen brokenbar section dieresis copyright ordfeminine guillemotleft logicalnot hyphen registered macron \
    degree plusminus twosuperior threesuperior acute mu paragraph periodcentered cedilla onesuperior ordmasculine guillemotright onequarter onehalf threequarters questiondown \
    Agrave Aacute Acircumflex Atilde Adieresis Aring AE Ccedilla Egrave Eacute Ecircumflex Edieresis Igrave Iacute Icircumflex Idieresis \
    Eth Ntilde Ograve Oacute Ocircumflex Otilde Odieresis multiply Oslash Ugrave Uacute Ucircumflex Udieresis Yacute Thorn germandbls \
    agrave aacute acircumflex atilde adieresis aring ae ccedilla egrave eacute ecircumflex edieresis igrave iacute icircumflex idieresis \
    eth ntilde ograve oacute ocircumflex otilde odieresis divide oslash ugrave uacute ucircumflex udieresis yacute thorn ydieresis";
/// Glyph names of the ASCII range that are not a letter of their own
const ASCII_NAMES: &[(&str, char)] = &[
    ("space", ' '),
    ("exclam", '!'),
    ("quotedbl", '"'),
    ("numbersign", '#'),
    ("dollar", '$'),
    ("percent", '%'),
    ("ampersand", '&'),
    ("quotesingle", '\''),
    ("parenleft", '('),
    ("parenright", ')'),
    ("asterisk", '*'),
    ("plus", '+'),
    ("comma", ','),
    ("hyphen", '-'),
    ("period", '.'),
    ("slash", '/'),
    ("zero", '0'),
    ("one", '1'),
    ("two", '2'),
    ("three", '3'),
    ("four", '4'),
    ("five", '5'),
    ("six", '6'),
    ("seven", '7'),
    ("eight", '8'),
    ("nine", '9'),
    ("colon", ':'),
    ("semicolon", ';'),
    ("less", '<'),
    ("equal", '='),
    ("greater", '>'),
    ("question", '?'),
    ("at", '@'),
    ("bracketleft", '['),
    ("backslash", '\\'),
    ("bracketright", ']'),
    ("asciicircum", '^'),
    ("underscore", '_'),
    ("grave", '`'),
    ("braceleft", '{'),
    ("bar", '|'),
    ("braceright", '}'),
    ("asciitilde", '~'),
];
/// Other glyph names that text is made of
const OTHER_NAMES: &[(&str, &str)] = &[
    ("quoteleft", "‘"),
    ("quoteright", "’"),
    ("quotedblleft", "“"),
    ("quotedblright", "”"),
    ("quotesinglbase", "‚"),
    ("quotedblbase", "„"),
    ("endash", "–"),
    ("emdash", "—"),
    ("bullet", "•"),
    ("ellipsis", "…"),
    ("dagger", "†"),
    ("daggerdbl", "‡"),
    ("perthousand", "‰"),
    ("guilsinglleft", "‹"),
    ("guilsinglright", "›"),
    ("fi", "fi"),
    ("fl", "fl"),
    ("ff", "ff"),
    ("ffi", "ffi"),
    ("ffl", "ffl"),
    ("trademark", "™"),
    ("Euro", "€"),
    ("florin", "ƒ"),
    ("circumflex", "ˆ"),
    ("tilde", "˜"),
    ("OE", "Œ"),
    ("oe", "œ"),
    ("Scaron", "Š"),
    ("scaron", "š"),
    ("Zcaron", "Ž"),
    ("zcaron", "ž"),
    ("Ydieresis", "Ÿ"),
    ("dotlessi", "ı"),
    ("minus", "−"),
    ("fraction", "⁄"),
    ("Lslash", "Ł"),
    ("lslash", "ł"),
    ("nbspace", " "),
    ("sfthyphen", "-"),
];

/// The text a glyph name stands for: a letter, a name of the standard sets, or "uni4E2D" and "u1F600"
fn glyph(name: &str) -> Option<String> {
    // "a.sc", "f_i": a variant of a glyph
    let base = name.split('.').next().unwrap_or(name);
    if base.chars().count() == 1 {
        return Some(base.to_string());
    }
    if let Some((_, c)) = ASCII_NAMES.iter().find(|(n, _)| *n == base) {
        return Some(c.to_string());
    }
    if let Some(i) = LATIN1_NAMES.split(' ').position(|n| n == base) {
        return char::from_u32(0xA0 + i as u32).map(String::from);
    }
    if let Some((_, s)) = OTHER_NAMES.iter().find(|(n, _)| *n == base) {
        return Some((*s).to_string());
    }
    let hex = base.strip_prefix("uni").filter(|h| h.len() % 4 == 0 && !h.is_empty()).or_else(|| base.strip_prefix('u').filter(|h| (4..=6).contains(&h.len())))?;
    if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    if base.starts_with("uni") {
        let units: Vec<u16> = (0..hex.len()).step_by(4).filter_map(|i| u16::from_str_radix(&hex[i..i + 4], 16).ok()).collect();
        return Some(char::decode_utf16(units).map(|c| c.unwrap_or('\u{FFFD}')).collect());
    }
    u32::from_str_radix(hex, 16).ok().and_then(char::from_u32).map(String::from)
}

/// What each code of a one-byte font stands for in an encoding of the standard sets
fn base_table(encoding: &str) -> Vec<Option<String>> {
    let mut table: Vec<Option<String>> = (0..256).map(|c| (0x20..0x7F).contains(&c).then(|| char::from(c as u8).to_string())).collect();
    match encoding {
        "MacRomanEncoding" => {
            for (i, c) in MAC_ROMAN.chars().enumerate() {
                table[0x80 + i] = Some(c.to_string());
            }
        }
        "StandardEncoding" => {
            table[0x27] = Some("’".into());
            table[0x60] = Some("‘".into());
        }
        // WinAnsi, and the fonts that don't say
        _ => {
            for (i, c) in CP1252_HIGH.chars().enumerate() {
                table[0x80 + i] = Some(c.to_string());
            }
            for i in 0..96 {
                table[0xA0 + i] = char::from_u32(0xA0 + i as u32).map(String::from);
            }
        }
    }
    table
}

// ───────────── ToUnicode maps ─────────────

#[derive(Default)]
struct Cmap {
    /// Bytes of a code
    width: usize,
    single: HashMap<u32, String>,
    /// (first code, last code, the text of the first code: the last character grows with the code)
    ranges: Vec<(u32, u32, Vec<u16>)>,
}

fn utf16(bytes: &[u8]) -> Vec<u16> {
    bytes.chunks(2).map(|c| if c.len() == 2 { u16::from_be_bytes([c[0], c[1]]) } else { u16::from(c[0]) }).collect()
}

fn code(bytes: &[u8]) -> u32 {
    bytes.iter().take(4).fold(0, |acc, &b| acc << 8 | u32::from(b))
}

impl Cmap {
    fn parse(data: &[u8]) -> Cmap {
        let mut map = Cmap::default();
        let mut lex = Lexer::new(data, 0);
        // The codes in front of the section being read
        let mut operands: Vec<Obj> = Vec::new();
        loop {
            let tok = lex.next();
            match tok {
                Tok::Eof => break,
                Tok::Str(s) => operands.push(Obj::Str(s)),
                Tok::ArrOpen => {
                    let mut items = Vec::new();
                    loop {
                        match lex.next() {
                            Tok::Str(s) => items.push(Obj::Str(s)),
                            Tok::ArrClose | Tok::Eof => break,
                            _ => {}
                        }
                    }
                    operands.push(Obj::Array(items));
                }
                Tok::Kw(b"endcodespacerange") => {
                    // Pairs of the first and last code
                    if let Some(Obj::Str(first)) = operands.first() {
                        map.width = first.len().clamp(1, 4);
                    }
                    operands.clear();
                }
                Tok::Kw(b"endbfchar") => {
                    for pair in operands.chunks(2) {
                        if let [Obj::Str(from), Obj::Str(to)] = pair
                            && map.single.len() < MAX_MAP
                        {
                            map.single.insert(code(from), String::from_utf16_lossy(&utf16(to)));
                        }
                    }
                    operands.clear();
                }
                Tok::Kw(b"endbfrange") => {
                    for triple in operands.chunks(3) {
                        match triple {
                            [Obj::Str(lo), Obj::Str(hi), Obj::Str(to)] if map.ranges.len() < MAX_MAP => map.ranges.push((code(lo), code(hi), utf16(to))),
                            [Obj::Str(lo), Obj::Str(_), Obj::Array(list)] => {
                                for (i, item) in list.iter().enumerate() {
                                    if let Obj::Str(to) = item
                                        && map.single.len() < MAX_MAP
                                    {
                                        map.single.insert(code(lo).wrapping_add(i as u32), String::from_utf16_lossy(&utf16(to)));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    operands.clear();
                }
                Tok::Kw(k) if k.starts_with(b"begin") || k.starts_with(b"end") => operands.clear(),
                _ => {}
            }
        }
        map.ranges.sort_by_key(|r| r.0);
        map
    }

    fn lookup(&self, c: u32) -> Option<String> {
        if let Some(s) = self.single.get(&c) {
            return Some(s.clone());
        }
        let at = self.ranges.partition_point(|r| r.0 <= c);
        // Ranges may overlap a little: the nearest ones are looked at
        self.ranges[..at].iter().rev().take(4).find(|r| c <= r.1).map(|(lo, _, to)| {
            let mut units = to.clone();
            if let Some(last) = units.last_mut() {
                *last = last.wrapping_add((c - lo) as u16);
            }
            String::from_utf16_lossy(&units)
        })
    }
}

// ───────────── Fonts ─────────────

/// How wide each code is, in thousandths of the font's size
#[derive(Default)]
enum Widths {
    #[default]
    Unknown,
    /// One-byte fonts: the widths from the first code on, and the width of a code that has none
    Simple { first: i64, list: Vec<f64>, missing: f64 },
    /// Fonts of two-byte codes: the widths listed by code, and the width of the others
    Cid { default: f64, map: HashMap<u32, f64> },
}

fn number(doc: &Doc, o: &Obj) -> Option<f64> {
    match doc.resolve(o) {
        Obj::Int(n) => Some(n as f64),
        Obj::Real(r) => Some(r),
        _ => None,
    }
}

/// The widths of a font of two-byte codes: "[c [w w w] c1 c2 w …]" and a default
fn cid_widths(doc: &Doc, font: &Dict) -> Widths {
    let Obj::Array(descendants) = doc.get(font, "DescendantFonts") else { return Widths::Unknown };
    let Some(Obj::Dict(cid)) = descendants.first().map(|d| doc.resolve(d)) else { return Widths::Unknown };
    let default = doc.get(&cid, "DW").int().map_or(1000.0, |n| n as f64);
    let mut map = HashMap::new();
    if let Obj::Array(list) = doc.get(&cid, "W") {
        let items: Vec<Obj> = list.iter().map(|o| doc.resolve(o)).collect();
        let mut i = 0;
        while i + 1 < items.len() && map.len() < MAX_MAP {
            let Some(first) = items[i].int().and_then(|n| u32::try_from(n).ok()) else { break };
            match &items[i + 1] {
                Obj::Array(widths) => {
                    for (k, w) in widths.iter().enumerate() {
                        if let Some(w) = number(doc, w) {
                            map.insert(first + k as u32, w);
                        }
                    }
                    i += 2;
                }
                last => {
                    let (Some(last), Some(w)) = (last.int().and_then(|n| u32::try_from(n).ok()), items.get(i + 2).and_then(|w| number(doc, w))) else { break };
                    for code in first..=last.min(first.saturating_add(70_000)) {
                        map.insert(code, w);
                    }
                    i += 3;
                }
            }
        }
    }
    Widths::Cid { default, map }
}

#[derive(Default)]
pub struct Font {
    widths: Widths,
    /// Codes of two bytes
    composite: bool,
    cmap: Option<Cmap>,
    /// One-byte fonts: the encoding
    table: Option<Vec<Option<String>>>,
    /// Two-byte codes that are UTF-16 themselves (a Unicode CMap named as the encoding)
    unicode: bool,
}

impl Font {
    pub fn load(doc: &Doc, dict: &Dict) -> Font {
        let mut font = Font::default();
        if let Some(stream) = dict.get("ToUnicode") {
            font.cmap = doc.stream(stream).map(|data| Cmap::parse(&data)).filter(|c| !c.single.is_empty() || !c.ranges.is_empty());
        }
        let composite = doc.get(dict, "Subtype").name() == Some("Type0");
        font.composite = composite;
        let encoding = doc.get(dict, "Encoding");
        if composite {
            // The codes of a Unicode CMap are the text (UCS2, UTF16); any other needs the font's own map
            font.unicode = encoding.name().is_some_and(|n| n.contains("UCS2") || n.contains("UTF16"));
            font.widths = cid_widths(doc, dict);
            return font;
        }
        let base = match &encoding {
            Obj::Name(n) => n.clone(),
            Obj::Dict(d) => doc.get(d, "BaseEncoding").name().unwrap_or_default().to_string(),
            _ => String::new(),
        };
        let mut table = base_table(&base);
        // The glyphs a font lists for codes: [code name name … code name …]
        if let Obj::Dict(d) = &encoding
            && let Obj::Array(list) = doc.get(d, "Differences")
        {
            let mut at = 0usize;
            for item in list {
                match doc.resolve(&item) {
                    Obj::Int(n) => at = usize::try_from(n).unwrap_or(0),
                    Obj::Name(name) => {
                        // A glyph that isn't known leaves the code as the base encoding has it
                        if let (true, Some(text)) = (at < 256, glyph(&name)) {
                            table[at] = Some(text);
                        }
                        at += 1;
                    }
                    _ => {}
                }
            }
        }
        font.table = Some(table);
        if let (Some(first), Obj::Array(list)) = (doc.get(dict, "FirstChar").int(), doc.get(dict, "Widths")) {
            let missing = match doc.get(dict, "FontDescriptor") {
                Obj::Dict(d) => number(doc, &doc.get(&d, "MissingWidth")).unwrap_or(0.0),
                _ => 0.0,
            };
            font.widths = Widths::Simple { first, list: list.iter().map(|w| number(doc, w).unwrap_or(0.0)).collect(), missing };
        }
        font
    }

    /// Whether the widths of its codes are known (the text can then be followed along the line)
    pub fn has_widths(&self) -> bool {
        !matches!(self.widths, Widths::Unknown)
    }

    /// Bytes of one code
    fn code_width(&self) -> usize {
        match &self.cmap {
            Some(c) if c.width > 0 => c.width,
            _ if self.composite => 2,
            _ => 1,
        }
    }

    /// How far a string moves the pen, in the units of `size` (the font size) and `spacing` (added after each code)
    pub fn advance(&self, bytes: &[u8], size: f64, spacing: f64) -> f64 {
        bytes
            .chunks(self.code_width())
            .map(|chunk| {
                let c = code(chunk);
                let w = match &self.widths {
                    Widths::Unknown => 0.0,
                    Widths::Simple { first, list, missing } => usize::try_from(i64::from(c) - first).ok().and_then(|i| list.get(i)).copied().unwrap_or(*missing),
                    Widths::Cid { default, map } => map.get(&c).copied().unwrap_or(*default),
                };
                w / 1000.0 * size + spacing
            })
            .sum()
    }

    /// The text of a string shown in this font
    pub fn decode(&self, bytes: &[u8], out: &mut String) {
        if let Some(cmap) = &self.cmap {
            let width = if cmap.width == 0 { if self.table.is_some() { 1 } else { 2 } } else { cmap.width };
            for chunk in bytes.chunks(width) {
                let c = code(chunk);
                match cmap.lookup(c) {
                    Some(s) => out.push_str(&s),
                    None => {
                        // A one-byte code the map doesn't list: the encoding's
                        if let (1, Some(table)) = (chunk.len(), &self.table)
                            && let Some(Some(s)) = table.get(c as usize)
                        {
                            out.push_str(s);
                        }
                    }
                }
            }
        } else if self.unicode {
            out.extend(char::decode_utf16(utf16(bytes)).filter_map(Result::ok));
        } else if let Some(table) = &self.table {
            for &b in bytes {
                if let Some(s) = &table[b as usize] {
                    out.push_str(s);
                }
            }
        }
    }
}
