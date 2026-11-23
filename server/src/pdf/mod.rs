//! PDF files, read and written: the objects (object.rs), the pages found in the page tree (here), and a writer that
//! builds a new file from copied page objects (write.rs). Content search reads the text of pages through this;
//! the page operations (ops.rs: reorder, rotate, delete, extract, split, combine) build new files with it.
//!
//! Nothing here re-encodes content: a page's streams are copied byte for byte, so quality and text stay as they are.

pub mod api;
pub mod object;
mod ops;
mod write;

#[cfg(test)]
mod api_tests;
#[cfg(test)]
mod tests;

use std::collections::HashSet;

use object::{Dict, Doc, Obj};
pub use ops::{Pick, arrange, combine, parse_ranges};

/// Pages read at most
pub const MAX_PAGES: usize = 20_000;
/// The header must start within this many bytes of the file's start
const HEADER_WITHIN: usize = 1024;
/// Page tree nodes inside nodes, at most
const MAX_TREE_DEPTH: usize = 32;
/// What a page inherits from the nodes above it
const INHERITED: [&str; 4] = ["Resources", "MediaBox", "CropBox", "Rotate"];

/// A page: the object number it has in the file (when the tree refers to it) and its dictionary, with what it inherits
/// from the page tree (resources, media box, crop box, rotation) put in
#[derive(Clone, Debug)]
pub struct Page {
    pub number: Option<u32>,
    pub dict: Dict,
}

/// Why a file can't be used. The messages are shown to people.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfError {
    NotPdf,
    Encrypted,
    NoPages,
    TooManyPages,
}

impl PdfError {
    pub fn message(self) -> &'static str {
        match self {
            PdfError::NotPdf => "This is not a PDF file, or it is damaged",
            PdfError::Encrypted => "This PDF is encrypted. Remove its password first, then try again",
            PdfError::NoPages => "No pages could be read from this PDF. It may be damaged",
            PdfError::TooManyPages => "This PDF has too many pages",
        }
    }
}

/// A PDF read for its pages
pub struct Opened<'a> {
    pub doc: Doc<'a>,
    pub pages: Vec<Page>,
}

/// Reads a file's objects and pages; refuses what isn't a PDF, is encrypted or has no pages that can be found
pub fn open(data: &[u8]) -> Result<Opened<'_>, PdfError> {
    let head = &data[..data.len().min(HEADER_WITHIN + 8)];
    if !head.windows(5).any(|w| w == b"%PDF-") {
        return Err(PdfError::NotPdf);
    }
    let doc = Doc::load(data);
    if doc.encrypted {
        return Err(PdfError::Encrypted);
    }
    let pages = pages(&doc);
    if pages.is_empty() {
        return Err(PdfError::NoPages);
    }
    if pages.len() >= MAX_PAGES {
        return Err(PdfError::TooManyPages);
    }
    Ok(Opened { doc, pages })
}

/// The pages, in the order the page tree gives them, each with what it inherits
pub fn pages(doc: &Doc) -> Vec<Page> {
    let mut out = Vec::new();
    if let Some(root) = doc.root
        && let Obj::Dict(catalog) = doc.object(root)
        && let Some(tree) = catalog.get("Pages")
    {
        walk(doc, tree, &Dict::new(), &mut out, &mut HashSet::new(), 0);
    }
    // No page tree that leads anywhere: every page there is, by object number
    if out.is_empty() {
        for number in doc.numbers() {
            if let Obj::Dict(d) = doc.object(number)
                && d.get("Type").and_then(Obj::name) == Some("Page")
            {
                out.push(Page { number: Some(number), dict: d });
                if out.len() >= MAX_PAGES {
                    break;
                }
            }
        }
    }
    out
}

fn walk(doc: &Doc, node: &Obj, inherited: &Dict, out: &mut Vec<Page>, seen: &mut HashSet<u32>, depth: usize) {
    if depth > MAX_TREE_DEPTH || out.len() >= MAX_PAGES {
        return;
    }
    let number = match node {
        Obj::Ref(n) => {
            if !seen.insert(*n) {
                return;
            }
            Some(*n)
        }
        _ => None,
    };
    let Obj::Dict(dict) = doc.resolve(node) else { return };
    let mut inherited = inherited.clone();
    for key in INHERITED {
        if let Some(value) = dict.get(key) {
            inherited.insert(key.to_string(), value.clone());
        }
    }
    match doc.get(&dict, "Kids") {
        Obj::Array(kids) => {
            for kid in &kids {
                walk(doc, kid, &inherited, out, seen, depth + 1);
            }
        }
        _ => {
            let mut dict = dict;
            for (key, value) in inherited {
                dict.entry(key).or_insert(value);
            }
            out.push(Page { number, dict });
        }
    }
}
