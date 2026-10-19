//! The objects of a PDF file: the lexer shared with page content, the parser, and the document found by scanning the
//! file for its objects (rather than following the cross-reference tables, which a damaged or updated file can get
//! wrong). Streams are decoded with a budget, so a small file that expands to gigabytes costs a bounded amount.

use std::{cell::Cell, collections::HashMap, io::Read, ops::Range};

/// Objects nest at most this deep (arrays and dictionaries inside each other)
const MAX_DEPTH: usize = 48;
/// What one stream may expand to
const STREAM_CAP: usize = 64 * 1024 * 1024;
/// What all the streams of one document may expand to
const TOTAL_BUDGET: usize = 256 * 1024 * 1024;
/// References followed in a chain at most
const MAX_REF_CHAIN: usize = 16;

pub type Dict = HashMap<String, Obj>;

#[derive(Clone, Debug, PartialEq)]
pub enum Obj {
    Null,
    Bool(bool),
    Int(i64),
    Real(f64),
    Str(Vec<u8>),
    Name(String),
    Array(Vec<Obj>),
    Dict(Dict),
    Ref(u32),
    /// A stream: its dictionary, and where its raw bytes are in the file
    Stream(Dict, Range<usize>),
}

impl Obj {
    pub fn name(&self) -> Option<&str> {
        match self {
            Obj::Name(n) => Some(n),
            _ => None,
        }
    }

    pub fn int(&self) -> Option<i64> {
        match self {
            Obj::Int(n) => Some(*n),
            Obj::Real(r) => Some(*r as i64),
            _ => None,
        }
    }
}

// ───────────── Tokens ─────────────

#[derive(Debug, PartialEq)]
pub enum Tok<'a> {
    Int(i64),
    Real(f64),
    Str(Vec<u8>),
    Name(String),
    ArrOpen,
    ArrClose,
    DictOpen,
    DictClose,
    Kw(&'a [u8]),
    Eof,
}

pub struct Lexer<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

fn is_space(b: u8) -> bool {
    matches!(b, 0 | 9 | 10 | 12 | 13 | 32)
}

fn is_delim(b: u8) -> bool {
    matches!(b, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

fn is_regular(b: u8) -> bool {
    !is_space(b) && !is_delim(b)
}

fn hex_value(b: u8) -> Option<u8> {
    (b as char).to_digit(16).map(|d| d as u8)
}

impl<'a> Lexer<'a> {
    pub fn new(data: &'a [u8], pos: usize) -> Self {
        Lexer { data, pos }
    }

    fn skip_space(&mut self) {
        while let Some(&b) = self.data.get(self.pos) {
            if is_space(b) {
                self.pos += 1;
            } else if b == b'%' {
                while self.data.get(self.pos).is_some_and(|&c| c != b'\n' && c != b'\r') {
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn regular(&mut self) -> &'a [u8] {
        let start = self.pos;
        while self.data.get(self.pos).is_some_and(|&b| is_regular(b)) {
            self.pos += 1;
        }
        &self.data[start..self.pos]
    }

    /// A string in parentheses, read from after the opening one
    fn literal(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut depth = 1;
        while let Some(&b) = self.data.get(self.pos) {
            self.pos += 1;
            match b {
                b'(' => {
                    depth += 1;
                    out.push(b);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(b);
                }
                b'\\' => {
                    let Some(&e) = self.data.get(self.pos) else { break };
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(8),
                        b'f' => out.push(12),
                        b'0'..=b'7' => {
                            let mut v = u32::from(e - b'0');
                            for _ in 0..2 {
                                match self.data.get(self.pos) {
                                    Some(&d @ b'0'..=b'7') => {
                                        v = v * 8 + u32::from(d - b'0');
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(v as u8);
                        }
                        // A line break after a backslash continues the string
                        b'\r' => {
                            if self.data.get(self.pos) == Some(&b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'\n' => {}
                        other => out.push(other),
                    }
                }
                _ => out.push(b),
            }
        }
        out
    }

    /// A string in angle brackets, read from after the opening one
    fn hex(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        let mut high: Option<u8> = None;
        while let Some(&b) = self.data.get(self.pos) {
            self.pos += 1;
            if b == b'>' {
                break;
            }
            if let Some(v) = hex_value(b) {
                match high.take() {
                    Some(h) => out.push(h << 4 | v),
                    None => high = Some(v),
                }
            }
        }
        // An odd digit stands for that digit followed by zero
        if let Some(h) = high {
            out.push(h << 4);
        }
        out
    }

    pub fn next(&mut self) -> Tok<'a> {
        self.skip_space();
        let Some(&b) = self.data.get(self.pos) else { return Tok::Eof };
        match b {
            b'(' => {
                self.pos += 1;
                Tok::Str(self.literal())
            }
            b'<' => {
                if self.data.get(self.pos + 1) == Some(&b'<') {
                    self.pos += 2;
                    Tok::DictOpen
                } else {
                    self.pos += 1;
                    Tok::Str(self.hex())
                }
            }
            b'>' => {
                self.pos += 1;
                if self.data.get(self.pos) == Some(&b'>') {
                    self.pos += 1;
                    Tok::DictClose
                } else {
                    self.next()
                }
            }
            b'[' => {
                self.pos += 1;
                Tok::ArrOpen
            }
            b']' => {
                self.pos += 1;
                Tok::ArrClose
            }
            b'/' => {
                self.pos += 1;
                let raw = self.regular();
                let mut name = Vec::with_capacity(raw.len());
                let mut i = 0;
                while i < raw.len() {
                    match (raw[i], raw.get(i + 1).copied().and_then(hex_value), raw.get(i + 2).copied().and_then(hex_value)) {
                        (b'#', Some(h), Some(l)) => {
                            name.push(h << 4 | l);
                            i += 3;
                        }
                        (c, ..) => {
                            name.push(c);
                            i += 1;
                        }
                    }
                }
                Tok::Name(String::from_utf8_lossy(&name).into_owned())
            }
            b')' | b'{' | b'}' => {
                self.pos += 1;
                self.next()
            }
            _ => {
                let word = self.regular();
                if word.is_empty() {
                    self.pos += 1;
                    return self.next();
                }
                match word[0] {
                    b'0'..=b'9' | b'+' | b'-' | b'.' => number(word).unwrap_or(Tok::Kw(word)),
                    _ => Tok::Kw(word),
                }
            }
        }
    }
}

fn number<'a>(word: &[u8]) -> Option<Tok<'a>> {
    let text = std::str::from_utf8(word).ok()?;
    if text.contains('.') {
        return text.parse::<f64>().ok().map(Tok::Real);
    }
    match text.parse::<i64>() {
        Ok(n) => Some(Tok::Int(n)),
        // Some writers put a sign twice or a minus inside; a number that can't be read is zero
        Err(_) => text.trim_start_matches(['+', '-']).parse::<f64>().ok().map(Tok::Real),
    }
}

// ───────────── Objects ─────────────

/// The object that starts at the lexer's position (not a stream: see `Doc::object_at`)
pub fn parse_obj(lex: &mut Lexer, depth: usize) -> Option<Obj> {
    if depth > MAX_DEPTH {
        return None;
    }
    let first = lex.next();
    parse_from(lex, first, depth)
}

fn parse_from(lex: &mut Lexer, first: Tok, depth: usize) -> Option<Obj> {
    Some(match first {
        Tok::Int(n) => {
            // "12 0 R"
            let saved = lex.pos;
            if n >= 0
                && let Tok::Int(g) = lex.next()
                && g >= 0
                && lex.next() == Tok::Kw(b"R")
            {
                return Some(Obj::Ref(n as u32));
            }
            lex.pos = saved;
            Obj::Int(n)
        }
        Tok::Real(r) => Obj::Real(r),
        Tok::Str(s) => Obj::Str(s),
        Tok::Name(n) => Obj::Name(n),
        Tok::ArrOpen => {
            let mut items = Vec::new();
            loop {
                match lex.next() {
                    Tok::ArrClose | Tok::Eof => break,
                    tok => items.push(parse_from(lex, tok, depth + 1)?),
                }
            }
            Obj::Array(items)
        }
        Tok::DictOpen => {
            let mut dict = Dict::new();
            loop {
                match lex.next() {
                    Tok::DictClose | Tok::Eof => break,
                    Tok::Name(key) => {
                        let value = parse_obj(lex, depth + 1)?;
                        dict.insert(key, value);
                    }
                    // Something that is not a key: left out
                    _ => {}
                }
            }
            Obj::Dict(dict)
        }
        Tok::Kw(b"true") => Obj::Bool(true),
        Tok::Kw(b"false") => Obj::Bool(false),
        Tok::Kw(_) => Obj::Null,
        Tok::ArrClose | Tok::DictClose | Tok::Eof => return None,
    })
}

// ───────────── The document ─────────────

pub struct Doc<'a> {
    pub data: &'a [u8],
    objects: HashMap<u32, Obj>,
    pub root: Option<u32>,
    pub encrypted: bool,
    /// What streams may still expand to
    budget: Cell<usize>,
}

fn find(data: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    let first = *needle.first()?;
    let mut i = from;
    while let Some(p) = data.get(i..)?.iter().position(|&b| b == first) {
        let at = i + p;
        if data[at..].starts_with(needle) {
            return Some(at);
        }
        i = at + 1;
    }
    None
}

/// The object number of an "N G obj" whose "obj" starts at `at`, when it is one
fn header_before(data: &[u8], at: usize) -> Option<u32> {
    let mut i = at;
    let skip_space = |i: &mut usize| {
        let before = *i;
        while *i > 0 && is_space(data[*i - 1]) {
            *i -= 1;
        }
        *i < before
    };
    let digits = |i: &mut usize| {
        let before = *i;
        while *i > 0 && data[*i - 1].is_ascii_digit() {
            *i -= 1;
        }
        before - *i
    };
    if !skip_space(&mut i) || digits(&mut i) == 0 || !skip_space(&mut i) {
        return None;
    }
    let end = i;
    if digits(&mut i) == 0 || end - i > 10 || (i > 0 && is_regular(data[i - 1])) {
        return None;
    }
    std::str::from_utf8(&data[i..end]).ok()?.parse().ok()
}

impl<'a> Doc<'a> {
    /// Finds the objects of a file by their "N G obj" headers, later ones replacing earlier (an updated file holds
    /// both), and unpacks the object streams. Never fails: what can't be read is not there.
    pub fn load(data: &'a [u8]) -> Doc<'a> {
        let mut doc = Doc { data, objects: HashMap::new(), root: None, encrypted: false, budget: Cell::new(TOTAL_BUDGET) };
        let mut found_root: Option<(usize, u32)> = None;
        let mut i = 0;
        while let Some(p) = find(data, b"obj", i) {
            i = p + 3;
            if data.get(p + 3).is_some_and(|b| b.is_ascii_alphanumeric()) {
                continue;
            }
            let Some(number) = header_before(data, p) else { continue };
            let mut lex = Lexer::new(data, p + 3);
            let Some(mut obj) = parse_obj(&mut lex, 0) else { continue };
            // A stream follows its dictionary
            if let Obj::Dict(dict) = &obj {
                let mut after = Lexer::new(data, lex.pos);
                if after.next() == Tok::Kw(b"stream") {
                    let (range, next) = stream_range(data, after.pos, dict);
                    i = next;
                    obj = Obj::Stream(dict.clone(), range);
                }
            }
            if let Obj::Stream(dict, range) = &obj {
                match dict.get("Type").and_then(Obj::name) {
                    Some("ObjStm") => doc.unpack(dict, range.clone()),
                    Some("XRef") => doc.trailer(dict, p, &mut found_root),
                    _ => {}
                }
            }
            doc.objects.insert(number, obj);
        }
        // Classic trailers
        let mut at = 0;
        while let Some(p) = find(data, b"trailer", at) {
            at = p + 7;
            if let Some(Obj::Dict(d)) = parse_obj(&mut Lexer::new(data, at), 0) {
                doc.trailer(&d, p, &mut found_root);
            }
        }
        doc.root = found_root.map(|r| r.1);
        doc
    }

    fn trailer(&mut self, dict: &Dict, at: usize, found_root: &mut Option<(usize, u32)>) {
        if dict.get("Encrypt").is_some_and(|e| *e != Obj::Null) {
            self.encrypted = true;
        }
        if let Some(Obj::Ref(r)) = dict.get("Root")
            && found_root.is_none_or(|(prev, _)| at >= prev)
        {
            *found_root = Some((at, *r));
        }
    }

    /// The objects an object stream holds
    fn unpack(&mut self, dict: &Dict, range: Range<usize>) {
        let (Some(count), Some(first)) = (dict.get("N").and_then(Obj::int), dict.get("First").and_then(Obj::int)) else { return };
        let Some(bytes) = self.decode(dict, range) else { return };
        let mut header = Lexer::new(&bytes, 0);
        let mut at = Vec::new();
        for _ in 0..count.clamp(0, 1_000_000) {
            let (Tok::Int(number), Tok::Int(offset)) = (header.next(), header.next()) else { break };
            at.push((number, offset));
        }
        for (number, offset) in at {
            let (Ok(number), Ok(start)) = (u32::try_from(number), usize::try_from(first + offset)) else { continue };
            if start >= bytes.len() {
                continue;
            }
            if let Some(obj) = parse_obj(&mut Lexer::new(&bytes, start), 0) {
                self.objects.insert(number, obj);
            }
        }
    }

    /// The object a reference points to, or the object itself; null when there is none
    pub fn resolve(&self, obj: &Obj) -> Obj {
        let mut current = obj;
        for _ in 0..MAX_REF_CHAIN {
            match current {
                Obj::Ref(n) => match self.objects.get(n) {
                    Some(next) => current = next,
                    None => return Obj::Null,
                },
                other => return other.clone(),
            }
        }
        Obj::Null
    }

    pub fn object(&self, number: u32) -> Obj {
        self.resolve(&Obj::Ref(number))
    }

    /// A dictionary entry with its reference followed
    pub fn get(&self, dict: &Dict, key: &str) -> Obj {
        dict.get(key).map_or(Obj::Null, |o| self.resolve(o))
    }

    /// Every object, by number
    pub fn numbers(&self) -> Vec<u32> {
        let mut all: Vec<u32> = self.objects.keys().copied().collect();
        all.sort_unstable();
        all
    }

    /// A stream's content with its filters undone, or None when it can't be (a kind of filter that holds no text, a
    /// damaged stream, the budget used up)
    pub fn stream(&self, obj: &Obj) -> Option<Vec<u8>> {
        match self.resolve(obj) {
            Obj::Stream(dict, range) => self.decode(&dict, range),
            _ => None,
        }
    }

    fn decode(&self, dict: &Dict, range: Range<usize>) -> Option<Vec<u8>> {
        let mut data = self.data.get(range)?.to_vec();
        let filters = match self.get(dict, "Filter") {
            Obj::Name(n) => vec![n],
            Obj::Array(a) => a.iter().filter_map(|f| self.resolve(f).name().map(str::to_owned)).collect(),
            _ => Vec::new(),
        };
        for filter in filters {
            data = match filter.as_str() {
                "FlateDecode" | "Fl" => {
                    let out = inflate(&data, self.budget.get().min(STREAM_CAP));
                    // Counted as it is made, so a stream that expands to little leaves the budget for the others
                    self.budget.set(self.budget.get().saturating_sub(out.len()));
                    if self.budget.get() == 0 {
                        return None;
                    }
                    out
                }
                "ASCIIHexDecode" | "AHx" => ascii_hex(&data),
                "ASCII85Decode" | "A85" => ascii85(&data),
                // Pictures, LZW and the rest: nothing to read
                _ => return None,
            };
        }
        Some(data)
    }
}

/// Where a stream's bytes are (`start`: just after the "stream" keyword) and where the next object may start
fn stream_range(data: &[u8], start: usize, dict: &Dict) -> (Range<usize>, usize) {
    let mut s = start;
    if data.get(s) == Some(&b'\r') {
        s += 1;
    }
    if data.get(s) == Some(&b'\n') {
        s += 1;
    }
    // The length given, when it is right (the keyword follows it)
    if let Some(len) = dict.get("Length").and_then(Obj::int).and_then(|l| usize::try_from(l).ok()) {
        let end = s.saturating_add(len);
        if end <= data.len() {
            let mut after = Lexer::new(data, end);
            if after.next() == Tok::Kw(b"endstream") {
                return (s..end, after.pos);
            }
        }
    }
    match find(data, b"endstream", s) {
        Some(p) => {
            let mut end = p;
            if end > s && data[end - 1] == b'\n' {
                end -= 1;
            }
            if end > s && data[end - 1] == b'\r' {
                end -= 1;
            }
            (s..end, p + 9)
        }
        // No end: the rest of the file
        None => (s..data.len(), data.len()),
    }
}

/// Inflates zlib data, keeping what could be read of a damaged stream
fn inflate(data: &[u8], cap: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut zlib = flate2::read::ZlibDecoder::new(data).take(cap as u64);
    if zlib.read_to_end(&mut out).is_err() && out.is_empty() {
        // Some writers leave out the zlib header
        let mut raw = flate2::read::DeflateDecoder::new(data).take(cap as u64);
        let _ = raw.read_to_end(&mut out);
    }
    out
}

fn ascii_hex(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut high = None;
    for &b in data {
        if b == b'>' {
            break;
        }
        if let Some(v) = hex_value(b) {
            match high.take() {
                Some(h) => out.push(h << 4 | v),
                None => high = Some(v),
            }
        }
    }
    if let Some(h) = high {
        out.push(h << 4);
    }
    out
}

fn ascii85(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut group = [0u32; 5];
    let mut n = 0;
    let flush = |group: &[u32; 5], n: usize, out: &mut Vec<u8>| {
        let mut padded = *group;
        padded[n..].fill(84);
        let value = padded.iter().fold(0u32, |acc, &d| acc.wrapping_mul(85).wrapping_add(d));
        out.extend_from_slice(&value.to_be_bytes()[..n - 1]);
    };
    let mut iter = data.iter().copied().skip_while(|&b| is_space(b)).peekable();
    // The optional "<~" before the data
    if iter.peek() == Some(&b'<') {
        iter.next();
        if iter.peek() == Some(&b'~') {
            iter.next();
        }
    }
    for b in iter {
        match b {
            b'~' => break,
            b'z' if n == 0 => out.extend_from_slice(&[0; 4]),
            b'!'..=b'u' => {
                group[n] = u32::from(b - b'!');
                n += 1;
                if n == 5 {
                    flush(&group, 5, &mut out);
                    n = 0;
                }
            }
            _ => {}
        }
    }
    if n > 1 {
        flush(&group, n, &mut out);
    }
    out
}
