//! Builds a new PDF from pages of other files. A page's objects (and every object they refer to) are copied with new
//! numbers; streams keep their bytes exactly as they are in the source, so nothing is decoded or encoded again. The
//! file gets a new page tree (one node holding every page), a new catalog, a cross-reference table and a trailer.

use std::collections::{HashMap, VecDeque};

use super::{
    Opened, Page,
    object::{Dict, Doc, Obj},
};

/// The catalog is object 1 and the page tree object 2
const CATALOG: u32 = 1;
const TREE: u32 = 2;
/// In a page being written, the reference to the new page tree (the number is no object of any source)
const NEW_TREE: u32 = u32::MAX;

/// What `Writer::add` takes of a source page
pub struct Take {
    /// Index in the source's pages
    pub page: usize,
    /// Degrees clockwise to add to the page's rotation (a multiple of 90)
    pub rotate: i32,
}

pub struct Writer {
    out: Vec<u8>,
    /// Where each object starts in `out`, by number (object 0 is free)
    offsets: Vec<usize>,
    /// The next object number
    next: u32,
    pages: Vec<u32>,
}

/// One source file being copied: the numbers its objects have in the new file, and what still has to be written
struct Copy<'a> {
    doc: &'a Doc<'a>,
    map: HashMap<u32, u32>,
    queue: VecDeque<(u32, Obj)>,
}

impl Writer {
    pub fn new() -> Writer {
        let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
        out.reserve(1 << 16);
        // Numbers 1 and 2 are written last, once the pages are known
        Writer { out, offsets: vec![0, 0, 0], next: 3, pages: Vec::new() }
    }

    /// Copies `takes` from `source`, in that order, to the end of the file being built
    pub fn add(&mut self, source: &Opened, takes: &[Take]) {
        let mut copy = Copy { doc: &source.doc, map: HashMap::new(), queue: VecDeque::new() };
        // Every page chosen gets its number before anything is written, so that a link from one page to another
        // (a destination, an annotation's page) leads to the new page, and to nothing when the page isn't kept
        let mut placed = Vec::with_capacity(takes.len());
        for take in takes {
            let Some(page) = source.pages.get(take.page) else { continue };
            let number = self.allocate();
            if let Some(old) = page.number {
                copy.map.entry(old).or_insert(number);
            }
            self.pages.push(number);
            placed.push((number, page, take.rotate));
        }
        for (number, page, rotate) in placed {
            let dict = page_dict(&copy, page, rotate);
            copy.queue.push_back((number, Obj::Dict(dict)));
            self.drain(&mut copy);
        }
    }

    fn allocate(&mut self) -> u32 {
        let n = self.next;
        self.next += 1;
        self.offsets.push(0);
        n
    }

    fn drain(&mut self, copy: &mut Copy) {
        while let Some((number, obj)) = copy.queue.pop_front() {
            self.offsets[number as usize] = self.out.len();
            self.out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            self.object(copy, &obj);
            self.out.extend_from_slice(b"\nendobj\n");
        }
    }

    fn object(&mut self, copy: &mut Copy, obj: &Obj) {
        match obj {
            Obj::Null => self.out.extend_from_slice(b"null"),
            Obj::Bool(b) => self.out.extend_from_slice(if *b { b"true" } else { b"false" }),
            Obj::Int(n) => self.out.extend_from_slice(n.to_string().as_bytes()),
            Obj::Real(r) => {
                let text = if r.is_finite() { format!("{r}") } else { "0".into() };
                self.out.extend_from_slice(text.as_bytes());
            }
            Obj::Str(s) => {
                self.out.push(b'<');
                for b in s {
                    self.out.extend_from_slice(format!("{b:02X}").as_bytes());
                }
                self.out.push(b'>');
            }
            Obj::Name(n) => name(&mut self.out, n),
            Obj::Array(items) => {
                self.out.push(b'[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        self.out.push(b' ');
                    }
                    self.object(copy, item);
                }
                self.out.push(b']');
            }
            Obj::Dict(dict) => self.dict(copy, dict),
            Obj::Ref(old) => match self.reference(copy, *old) {
                Some(n) => self.out.extend_from_slice(format!("{n} 0 R").as_bytes()),
                None => self.out.extend_from_slice(b"null"),
            },
            Obj::Stream(dict, range) => {
                let data = copy.doc.data.get(range.clone()).unwrap_or(&[]);
                let mut dict = dict.clone();
                dict.insert("Length".into(), Obj::Int(data.len() as i64));
                self.dict(copy, &dict);
                self.out.extend_from_slice(b"\nstream\n");
                self.out.extend_from_slice(data);
                self.out.extend_from_slice(b"\nendstream");
            }
        }
    }

    fn dict(&mut self, copy: &mut Copy, dict: &Dict) {
        self.out.extend_from_slice(b"<<");
        // In key order, so the same input gives the same file
        let mut keys: Vec<&String> = dict.keys().collect();
        keys.sort();
        for key in keys {
            self.out.push(b' ');
            name(&mut self.out, key);
            self.out.push(b' ');
            self.object(copy, &dict[key]);
        }
        self.out.extend_from_slice(b" >>");
    }

    /// The number the object `old` of the source has in the new file: a new one the first time it is met (its content
    /// is written later). Pages that are not kept, and nodes of the old page tree, lead nowhere (the page tree is
    /// new, and following them would bring in the rest of the file).
    fn reference(&mut self, copy: &mut Copy, old: u32) -> Option<u32> {
        if old == NEW_TREE {
            return Some(TREE);
        }
        if let Some(n) = copy.map.get(&old) {
            return Some(*n);
        }
        let obj = copy.doc.object(old);
        if matches!(obj, Obj::Null) {
            return None;
        }
        if let Obj::Dict(d) = &obj
            && matches!(d.get("Type").and_then(Obj::name), Some("Page" | "Pages"))
        {
            return None;
        }
        let n = self.allocate();
        copy.map.insert(old, n);
        copy.queue.push_back((n, obj));
        Some(n)
    }

    /// The finished file
    pub fn finish(mut self) -> Vec<u8> {
        self.offsets[CATALOG as usize] = self.out.len();
        self.out.extend_from_slice(format!("{CATALOG} 0 obj\n<< /Type /Catalog /Pages {TREE} 0 R >>\nendobj\n").as_bytes());
        self.offsets[TREE as usize] = self.out.len();
        let kids: Vec<String> = self.pages.iter().map(|n| format!("{n} 0 R")).collect();
        let tree = format!("{TREE} 0 obj\n<< /Type /Pages /Kids [{}] /Count {} >>\nendobj\n", kids.join(" "), self.pages.len());
        self.out.extend_from_slice(tree.as_bytes());
        let xref = self.out.len();
        let size = self.offsets.len();
        let mut table = format!("xref\n0 {size}\n0000000000 65535 f \n");
        for offset in &self.offsets[1..] {
            table.push_str(&format!("{offset:010} 00000 n \n"));
        }
        table.push_str(&format!("trailer\n<< /Size {size} /Root {CATALOG} 0 R >>\nstartxref\n{xref}\n%%EOF\n"));
        self.out.extend_from_slice(table.as_bytes());
        self.out
    }
}

/// The dictionary of a page as it is written: its parent is the new page tree, and its rotation is turned by `rotate`
fn page_dict(copy: &Copy, page: &Page, rotate: i32) -> Dict {
    let mut dict = page.dict.clone();
    dict.insert("Type".into(), Obj::Name("Page".into()));
    dict.insert("Parent".into(), Obj::Ref(NEW_TREE));
    if rotate != 0 {
        let current = page.dict.get("Rotate").map_or(0, |r| copy.doc.resolve(r).int().unwrap_or(0));
        dict.insert("Rotate".into(), Obj::Int((current + i64::from(rotate)).rem_euclid(360)));
    }
    dict
}

/// A name, with the characters that can't stand in one written as #xx
fn name(out: &mut Vec<u8>, name: &str) {
    out.push(b'/');
    for &b in name.as_bytes() {
        if b.is_ascii_alphanumeric() || b"-_.+,*!$&@^~|`?:;=".contains(&b) {
            out.push(b);
        } else {
            out.extend_from_slice(format!("#{b:02X}").as_bytes());
        }
    }
}
