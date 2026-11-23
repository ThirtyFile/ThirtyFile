//! The page operations: a new file of chosen pages (kept, reordered, turned), and several files put together. Extracting
//! and splitting are these with the pages a range names (`parse_ranges`).

use super::{
    Opened,
    write::{Take, Writer},
};

/// A page of the result: which page of the source (0-based), and how many degrees to turn it
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Pick {
    pub page: usize,
    pub rotate: i32,
}

/// A file of `picks` from `source`, in that order. A page may be picked twice. Empty picks make no file.
pub fn arrange(source: &Opened, picks: &[Pick]) -> Result<Vec<u8>, &'static str> {
    if picks.is_empty() {
        return Err("Choose at least one page");
    }
    if picks.iter().any(|p| p.page >= source.pages.len()) {
        return Err("A page that is not in the PDF was chosen");
    }
    if picks.iter().any(|p| p.rotate % 90 != 0) {
        return Err("Pages can only be turned by quarter turns");
    }
    let takes: Vec<Take> = picks.iter().map(|p| Take { page: p.page, rotate: p.rotate }).collect();
    let mut writer = Writer::new();
    writer.add(source, &takes);
    Ok(writer.finish())
}

/// One file of all the pages of each of `sources`, one after the other
pub fn combine(sources: &[Opened]) -> Result<Vec<u8>, &'static str> {
    if sources.is_empty() {
        return Err("Choose at least one PDF");
    }
    let mut writer = Writer::new();
    for source in sources {
        let takes: Vec<Take> = (0..source.pages.len()).map(|page| Take { page, rotate: 0 }).collect();
        writer.add(source, &takes);
    }
    Ok(writer.finish())
}

/// Page ranges as people write them ("1-3, 5, 8-"), for a file of `count` pages: each range as inclusive 0-based
/// bounds, in the order written. An open end runs to the last page, an open start from the first ("-3").
pub fn parse_ranges(text: &str, count: usize) -> Result<Vec<(usize, usize)>, String> {
    let mut out = Vec::new();
    for part in text.split([',', ';']).map(str::trim).filter(|p| !p.is_empty()) {
        let (from, to) = match part.split_once('-') {
            Some((a, b)) => (a.trim(), b.trim()),
            None => (part, part),
        };
        let number = |s: &str, default: usize| -> Result<usize, String> {
            if s.is_empty() {
                return Ok(default);
            }
            s.parse::<usize>().map_err(|_| format!("\"{part}\" is not a page range"))
        };
        let (from, to) = (number(from, 1)?, number(to, count)?);
        if from == 0 || to == 0 || from > to {
            return Err(format!("\"{part}\" is not a page range"));
        }
        if to > count {
            return Err(format!("The PDF has {count} pages, but \"{part}\" goes past the last"));
        }
        out.push((from - 1, to - 1));
    }
    if out.is_empty() {
        return Err("Enter the pages to take, like 1-3, 5".into());
    }
    Ok(out)
}
