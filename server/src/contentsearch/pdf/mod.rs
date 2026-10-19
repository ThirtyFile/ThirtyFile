//! The text layer of a PDF: the strings its pages show, in page order, with the fonts' own maps turning codes into
//! characters. There is no layout: text that is shown one place after another comes out in the order the page draws
//! it, which for the programs that write documents is reading order.
//!
//! The file is read by scanning for its objects (object.rs), so a damaged table of cross-references or an updated file
//! is no trouble; what can't be read is left out, and nothing in a file can make this do more than a fixed amount of
//! work (the sizes streams expand to, the number of pages, the depth of forms and the count of operators are limited).
//! Encrypted files are not read, and pages that are pictures (scans) hold no text to find.

mod font;
mod object;

#[cfg(test)]
pub(in crate::contentsearch) mod tests;

use std::{collections::HashMap, collections::HashSet, rc::Rc};

use font::Font;
use object::{Dict, Doc, Lexer, Obj, Tok};

use super::extract::MAX_BYTES;

/// Pages read at most
const MAX_PAGES: usize = 20_000;
/// Forms (pieces of a page drawn by another stream) inside forms, at most
const MAX_FORM_DEPTH: usize = 4;
/// Operands and operators looked at in all, at most
const MAX_TOKENS: usize = 60_000_000;
/// The header must start within this many bytes of the file's start
const HEADER_WITHIN: usize = 1024;

/// The text of a PDF, squashed; the reason when it can't be read
pub fn text(data: &[u8]) -> Result<String, &'static str> {
    let head = &data[..data.len().min(HEADER_WITHIN + 8)];
    if !head.windows(5).any(|w| w == b"%PDF-") {
        return Err("It is not a PDF");
    }
    let doc = Doc::load(data);
    if doc.encrypted {
        return Err("It is encrypted");
    }
    let pages = pages(&doc);
    if pages.is_empty() {
        return Err("It holds no pages");
    }
    let mut reader = Reader { doc: &doc, out: String::new(), fonts: HashMap::new(), tokens: MAX_TOKENS, active: Vec::new(), full: false };
    for (page, resources) in &pages {
        if reader.full || reader.tokens == 0 {
            break;
        }
        reader.page(page, resources);
    }
    Ok(tidy(&reader.out))
}

/// The pages, in the order the page tree gives them, each with the resources it inherits
fn pages(doc: &Doc) -> Vec<(Dict, Obj)> {
    let mut out = Vec::new();
    if let Some(root) = doc.root
        && let Obj::Dict(catalog) = doc.object(root)
        && let Some(tree) = catalog.get("Pages")
    {
        walk(doc, tree, &Obj::Null, &mut out, &mut HashSet::new(), 0);
    }
    // No page tree that leads anywhere: every page there is, by object number
    if out.is_empty() {
        for number in doc.numbers() {
            if let Obj::Dict(d) = doc.object(number)
                && d.get("Type").and_then(Obj::name) == Some("Page")
            {
                let resources = d.get("Resources").cloned().unwrap_or(Obj::Null);
                out.push((d, resources));
                if out.len() >= MAX_PAGES {
                    break;
                }
            }
        }
    }
    out
}

fn walk(doc: &Doc, node: &Obj, inherited: &Obj, out: &mut Vec<(Dict, Obj)>, seen: &mut HashSet<u32>, depth: usize) {
    if depth > 32 || out.len() >= MAX_PAGES {
        return;
    }
    if let Obj::Ref(n) = node
        && !seen.insert(*n)
    {
        return;
    }
    let Obj::Dict(dict) = doc.resolve(node) else { return };
    let resources = dict.get("Resources").cloned().unwrap_or_else(|| inherited.clone());
    match doc.get(&dict, "Kids") {
        Obj::Array(kids) => {
            for kid in &kids {
                walk(doc, kid, &resources, out, seen, depth + 1);
            }
        }
        _ => out.push((dict, resources)),
    }
}

/// Where the text is on the page: the matrix of the text and of its line, the size and spacing of the font, and where the
/// last string ended. Strings shown one after another with no move between them are one run of text; after a move the
/// text is told apart from what came before by how far it is from where that ended.
struct TextState {
    tm: [f64; 6],
    tlm: [f64; 6],
    size: f64,
    char_space: f64,
    leading: f64,
    pen: Option<(f64, f64)>,
    /// The text moved since the last string
    moved: bool,
}

impl TextState {
    fn new() -> Self {
        TextState { tm: IDENTITY, tlm: IDENTITY, size: 0.0, char_space: 0.0, leading: 0.0, pen: None, moved: false }
    }

    /// Td: the next line starts at an offset from this one
    fn translate(&mut self, tx: f64, ty: f64) {
        let [a, b, c, d, e, f] = self.tlm;
        self.tlm = [a, b, c, d, tx * a + ty * c + e, tx * b + ty * d + f];
        self.tm = self.tlm;
        self.moved = true;
    }

    /// Whether the text starts apart from where the last string ended: on another line, or with a gap of more than a
    /// seventh of the size (a space is a third or a quarter). Fonts without widths can't be followed: it always does.
    fn apart(&self, font: &Font) -> bool {
        let Some((pen_x, pen_y)) = self.pen else { return true };
        let [a, b, _, _, e, f] = self.tm;
        let along = a.hypot(b);
        let em = self.size.abs() * along;
        if !font.has_widths() || along == 0.0 || em == 0.0 {
            return true;
        }
        let (dx, dy) = (e - pen_x, f - pen_y);
        let base = (dx * a + dy * b) / along;
        let across = (-dx * b + dy * a) / along;
        across.abs() > 0.5 * em || base > em / 7.0 || base < -0.3 * em
    }
}

const IDENTITY: [f64; 6] = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// What the operators of a content stream take
enum Operand {
    Num(f64),
    Str(Vec<u8>),
    Name(String),
    Array(Vec<Operand>),
}

struct Reader<'d, 'a> {
    doc: &'d Doc<'a>,
    out: String,
    /// Fonts made so far, by the number of their object
    fonts: HashMap<u32, Rc<Font>>,
    tokens: usize,
    /// The forms being read, so one that draws itself is read once
    active: Vec<u32>,
    /// The text kept has reached its limit
    full: bool,
}

impl Reader<'_, '_> {
    fn page(&mut self, page: &Dict, resources: &Obj) {
        let streams = match self.doc.get(page, "Contents") {
            Obj::Array(parts) => parts,
            one => vec![one],
        };
        let mut content = Vec::new();
        for part in &streams {
            if let Some(bytes) = self.doc.stream(part) {
                content.extend_from_slice(&bytes);
                // A token never goes on from one stream into the next
                content.push(b'\n');
            }
        }
        self.run(&content, resources, 0);
    }

    fn push(&mut self, s: &str) {
        if self.full {
            return;
        }
        if self.out.len() + s.len() > MAX_BYTES {
            self.full = true;
            return;
        }
        self.out.push_str(s);
    }

    fn space(&mut self) {
        if !self.out.is_empty() && !self.out.ends_with(' ') {
            self.push(" ");
        }
    }

    fn font(&mut self, resources: &Dict, name: &str) -> Option<Rc<Font>> {
        let Obj::Dict(fonts) = self.doc.get(resources, "Font") else { return None };
        let entry = fonts.get(name)?;
        if let Obj::Ref(n) = entry
            && let Some(known) = self.fonts.get(n)
        {
            return Some(known.clone());
        }
        let Obj::Dict(dict) = self.doc.resolve(entry) else { return None };
        let font = Rc::new(Font::load(self.doc, &dict));
        if let Obj::Ref(n) = entry {
            self.fonts.insert(*n, font.clone());
        }
        Some(font)
    }

    /// Shows a string: a space first when the text moved away from where the last string ended, and then the string,
    /// after which the pen is at its end
    fn show(&mut self, st: &mut TextState, font: Option<&Rc<Font>>, bytes: &[u8]) {
        let Some(font) = font else { return };
        if std::mem::take(&mut st.moved) && st.apart(font) {
            self.space();
        }
        let mut text = String::new();
        font.decode(bytes, &mut text);
        self.push(&text);
        let advance = font.advance(bytes, st.size, st.char_space);
        st.tm[4] += advance * st.tm[0];
        st.tm[5] += advance * st.tm[1];
        st.pen = Some((st.tm[4], st.tm[5]));
    }

    /// Runs a content stream: the operators that show text, and the ones that move to somewhere else
    fn run(&mut self, content: &[u8], resources: &Obj, depth: usize) {
        let Obj::Dict(res) = self.doc.resolve(resources) else { return };
        let mut lex = Lexer::new(content, 0);
        let mut stack: Vec<Operand> = Vec::new();
        let mut font: Option<Rc<Font>> = None;
        let mut st = TextState::new();
        while !self.full && self.tokens > 0 {
            self.tokens -= 1;
            match lex.next() {
                Tok::Eof => break,
                Tok::Int(n) => stack.push(Operand::Num(n as f64)),
                Tok::Real(r) => stack.push(Operand::Num(r)),
                Tok::Str(s) => stack.push(Operand::Str(s)),
                Tok::Name(n) => stack.push(Operand::Name(n)),
                Tok::ArrOpen => stack.push(Operand::Array(array(&mut lex))),
                Tok::DictOpen => skip_dict(&mut lex),
                Tok::ArrClose | Tok::DictClose => {}
                Tok::Kw(op) => {
                    self.operator(op, &stack, &res, &mut font, &mut st, &mut lex, depth);
                    stack.clear();
                }
            }
            // A stream of operands with no operator would grow this without end
            if stack.len() > 1024 {
                stack.clear();
            }
        }
    }

    #[allow(clippy::too_many_arguments, reason = "the state of one content stream, passed on as it is")]
    fn operator(&mut self, op: &[u8], stack: &[Operand], res: &Dict, font: &mut Option<Rc<Font>>, st: &mut TextState, lex: &mut Lexer, depth: usize) {
        let nums: Vec<f64> = stack.iter().filter_map(|o| if let Operand::Num(n) = o { Some(*n) } else { None }).collect();
        match op {
            b"Tf" => {
                if let Some(Operand::Name(n)) = stack.iter().find(|o| matches!(o, Operand::Name(_))) {
                    *font = self.font(res, n);
                }
                st.size = nums.last().copied().unwrap_or(st.size);
            }
            b"Tc" => st.char_space = nums.last().copied().unwrap_or(0.0),
            b"TL" => st.leading = nums.last().copied().unwrap_or(0.0),
            b"Tm" => {
                if let [.., a, b, c, d, e, f] = nums[..] {
                    st.tm = [a, b, c, d, e, f];
                    st.tlm = st.tm;
                    st.moved = true;
                }
            }
            b"Td" | b"TD" => {
                if let [.., tx, ty] = nums[..] {
                    if op == b"TD" {
                        st.leading = -ty;
                    }
                    st.translate(tx, ty);
                }
            }
            b"T*" => st.translate(0.0, -st.leading),
            b"BT" => {
                *st = TextState { size: st.size, char_space: st.char_space, leading: st.leading, ..TextState::new() };
                self.space();
            }
            b"ET" => self.space(),
            b"Tj" => {
                if let Some(Operand::Str(s)) = stack.last() {
                    self.show(st, font.as_ref(), s);
                }
            }
            b"'" | b"\"" => {
                st.translate(0.0, -st.leading);
                if let Some(Operand::Str(s)) = stack.last() {
                    self.show(st, font.as_ref(), s);
                }
            }
            b"TJ" => {
                if let Some(Operand::Array(items)) = stack.last() {
                    for item in items {
                        match item {
                            Operand::Str(s) => self.show(st, font.as_ref(), s),
                            Operand::Num(n) => {
                                // A number moves the next string along (to the left when it is positive); a move to the
                                // right of a fifth of the text's size or more is a space
                                st.tm[4] -= n / 1000.0 * st.size * st.tm[0];
                                st.tm[5] -= n / 1000.0 * st.size * st.tm[1];
                                if *n < -200.0 {
                                    self.space();
                                }
                            }
                            _ => {}
                        }
                    }
                }
            }
            b"Do" => {
                if let Some(Operand::Name(n)) = stack.last() {
                    self.space();
                    self.form(res, n, depth);
                    self.space();
                }
            }
            b"BI" => skip_inline_image(lex),
            _ => {}
        }
    }

    /// A form: a piece of a page another stream draws
    fn form(&mut self, resources: &Dict, name: &str, depth: usize) {
        if depth >= MAX_FORM_DEPTH {
            return;
        }
        let Obj::Dict(xobjects) = self.doc.get(resources, "XObject") else { return };
        let Some(entry) = xobjects.get(name) else { return };
        let number = if let Obj::Ref(n) = entry { Some(*n) } else { None };
        if number.is_some_and(|n| self.active.contains(&n)) {
            return;
        }
        let Obj::Stream(dict, _) = self.doc.resolve(entry) else { return };
        if dict.get("Subtype").and_then(Obj::name) != Some("Form") {
            return;
        }
        let Some(content) = self.doc.stream(entry) else { return };
        let inner = dict.get("Resources").cloned().unwrap_or_else(|| Obj::Dict(resources.clone()));
        self.active.extend(number);
        self.run(&content, &inner, depth + 1);
        if number.is_some() {
            self.active.pop();
        }
    }
}

/// The strings and numbers of an array of a content stream, from after its "["
fn array(lex: &mut Lexer) -> Vec<Operand> {
    let mut items = Vec::new();
    loop {
        match lex.next() {
            Tok::ArrClose | Tok::Eof => break,
            Tok::Str(s) => items.push(Operand::Str(s)),
            Tok::Int(n) => items.push(Operand::Num(n as f64)),
            Tok::Real(r) => items.push(Operand::Num(r)),
            _ => {}
        }
    }
    items
}

/// A dictionary in a content stream (properties of marked content), from after its "<<"
fn skip_dict(lex: &mut Lexer) {
    let mut depth = 1;
    while depth > 0 {
        match lex.next() {
            Tok::DictOpen => depth += 1,
            Tok::DictClose => depth -= 1,
            Tok::Eof => break,
            _ => {}
        }
    }
}

/// An inline picture: its settings up to "ID", then bytes that are not operators, up to a lone "EI"
fn skip_inline_image(lex: &mut Lexer) {
    loop {
        match lex.next() {
            Tok::Kw(b"ID") => break,
            Tok::Eof => return,
            _ => {}
        }
    }
    let data = lex.data;
    let mut at = lex.pos;
    while let Some(p) = data.get(at..).and_then(|rest| rest.windows(2).position(|w| w == b"EI")) {
        let found = at + p;
        let before = found == 0 || data[found - 1].is_ascii_whitespace();
        let after = data.get(found + 2).is_none_or(|b| b.is_ascii_whitespace());
        if before && after {
            lex.pos = found + 2;
            return;
        }
        at = found + 2;
    }
    lex.pos = data.len();
}

fn is_cjk(c: char) -> bool {
    matches!(c, '\u{2E80}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}' | '\u{FF00}'..='\u{FFEF}' | '\u{20000}'..='\u{2FFFF}')
}

/// The text as it is kept: ligatures as their letters, no control characters or soft hyphens, one space between words,
/// and none between Chinese or Japanese characters (a line ends there, but the words don't)
fn tidy(raw: &str) -> String {
    let mut plain = String::with_capacity(raw.len());
    for c in raw.chars() {
        match c {
            '\u{FB00}' => plain.push_str("ff"),
            '\u{FB01}' => plain.push_str("fi"),
            '\u{FB02}' => plain.push_str("fl"),
            '\u{FB03}' => plain.push_str("ffi"),
            '\u{FB04}' => plain.push_str("ffl"),
            '\u{FB05}' | '\u{FB06}' => plain.push_str("st"),
            '\u{AD}' | '\u{FFFD}' => {}
            c if c.is_control() || c == '\u{A0}' => plain.push(' '),
            c => plain.push(c),
        }
    }
    let squashed = plain.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = squashed.chars().collect();
    let mut out = String::with_capacity(squashed.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == ' ' && i > 0 && chars.get(i + 1).is_some_and(|&next| is_cjk(chars[i - 1]) && is_cjk(next)) {
            continue;
        }
        out.push(c);
    }
    out
}
