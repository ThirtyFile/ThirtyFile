//! Reading the text of a file: plain text and code, web pages, and the Office documents (DOCX, XLSX, PPTX and the Open
//! Document formats). The text is squashed to single spaces, so a phrase matches across line breaks, and cut off after
//! `MAX_BYTES`. Pictures and scanned pages hold no text this reads, and PDFs are not read yet.

use quick_xml::{Reader, events::Event};

use super::zipread::{Archive, Entry};

/// The most text kept from one file
pub const MAX_BYTES: usize = 3_000_000;
/// What one XML part of a document may expand to before the rest of it is left unread
const XML_CAP: usize = 48 * 1024 * 1024;
/// Parts read from one document at most (slides, headers)
const MAX_PARTS: usize = 2_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Plain text, Markdown, CSV, code, logs
    Text,
    /// HTML and XML: the text between the tags
    Markup,
    Docx,
    Xlsx,
    Pptx,
    /// OpenDocument text, spreadsheet and presentation
    Odf,
}

const TEXT_EXT: &[&str] = &[
    "txt",
    "text",
    "md",
    "markdown",
    "mdx",
    "rst",
    "adoc",
    "org",
    "tex",
    "csv",
    "tsv",
    "json",
    "jsonl",
    "yaml",
    "yml",
    "toml",
    "ini",
    "cfg",
    "conf",
    "env",
    "properties",
    "log",
    "sql",
    "srt",
    "vtt",
    "ass",
    "diff",
    "patch",
    "js",
    "mjs",
    "cjs",
    "jsx",
    "ts",
    "tsx",
    "css",
    "scss",
    "sass",
    "less",
    "rs",
    "py",
    "rb",
    "go",
    "java",
    "kt",
    "kts",
    "scala",
    "swift",
    "c",
    "h",
    "cc",
    "cpp",
    "cxx",
    "hpp",
    "hh",
    "cs",
    "fs",
    "vb",
    "php",
    "pl",
    "pm",
    "lua",
    "r",
    "dart",
    "sh",
    "bash",
    "zsh",
    "fish",
    "bat",
    "cmd",
    "ps1",
    "psm1",
    "gradle",
    "cmake",
    "mk",
    "make",
    "dockerfile",
    "tf",
    "vue",
    "svelte",
    "graphql",
    "gql",
    "proto",
    "ex",
    "exs",
    "erl",
    "hs",
    "ml",
    "clj",
    "nim",
    "zig",
    "vim",
    "gitignore",
    "editorconfig",
    "lock",
];
const MARKUP_EXT: &[&str] = &["html", "htm", "xhtml", "xml", "svg", "rss", "atom", "xsl", "xslt", "opml"];
/// Names without an extension that hold text
const TEXT_NAMES: &[&str] = &["readme", "license", "licence", "copying", "authors", "changelog", "makefile", "dockerfile", "notice", "todo"];

/// The kind of file a name (and type) says it is, when it is one that is read
pub fn kind_of(name: &str, mime: &str) -> Option<Kind> {
    let lower = name.to_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e);
    match ext {
        Some("docx" | "docm" | "dotx") => return Some(Kind::Docx),
        Some("xlsx" | "xlsm" | "xltx") => return Some(Kind::Xlsx),
        Some("pptx" | "pptm" | "potx" | "ppsx") => return Some(Kind::Pptx),
        Some("odt" | "ods" | "odp") => return Some(Kind::Odf),
        Some(e) if MARKUP_EXT.contains(&e) => return Some(Kind::Markup),
        Some(e) if TEXT_EXT.contains(&e) => return Some(Kind::Text),
        _ => {}
    }
    if ext.is_none() && TEXT_NAMES.contains(&lower.as_str()) {
        return Some(Kind::Text);
    }
    let mime = mime.split(';').next().unwrap_or_default().trim().to_ascii_lowercase();
    (mime.starts_with("text/") && mime != "text/html" && mime != "text/xml").then_some(Kind::Text)
}

/// Adds words to `out` separated by single spaces, up to `MAX_BYTES`; false once it is full
fn push_words(out: &mut String, text: &str) -> bool {
    for word in text.split_whitespace() {
        if out.len() + word.len() + 1 > MAX_BYTES {
            return false;
        }
        if !out.is_empty() && !out.ends_with(' ') {
            out.push(' ');
        }
        out.push_str(word);
    }
    true
}

/// A space between what comes before and after, unless there already is one; false once `out` is full
fn space(out: &mut String) -> bool {
    if !out.is_empty() && !out.ends_with(' ') {
        if out.len() + 1 > MAX_BYTES {
            return false;
        }
        out.push(' ');
    }
    true
}

/// Text in any of the encodings a text file is likely to have: UTF-8 (with or without a mark) and UTF-16 with a mark.
/// None for what isn't text (it holds zero bytes, or too much of it can't be read as UTF-8).
fn decode(data: &[u8]) -> Option<String> {
    let utf16 = |rest: &[u8], big: bool| {
        let units = rest.as_chunks::<2>().0.iter().map(|c| if big { u16::from_be_bytes(*c) } else { u16::from_le_bytes(*c) });
        char::decode_utf16(units).map(|c| c.unwrap_or('\u{FFFD}')).collect::<String>()
    };
    if let Some(rest) = data.strip_prefix(&[0xFF, 0xFE]) {
        return Some(utf16(rest, false));
    }
    if let Some(rest) = data.strip_prefix(&[0xFE, 0xFF]) {
        return Some(utf16(rest, true));
    }
    let data = data.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(data);
    if data[..data.len().min(8192)].contains(&0) {
        return None;
    }
    let text = String::from_utf8_lossy(data);
    let broken = text.chars().filter(|&c| c == '\u{FFFD}').count();
    // A few bad bytes are a damaged file; many are another encoding, whose letters would be found as nonsense
    (broken <= 2 || broken * 100 <= text.chars().count()).then(|| text.into_owned())
}

/// The text between the tags of a page or XML file (not what scripts and styles hold, nor comments)
fn strip_markup(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let lower = html.to_ascii_lowercase();
    let mut i = 0;
    while i < html.len() {
        // Up to the next tag in one step
        let Some(open) = html[i..].find('<').map(|p| i + p) else {
            out.push_str(&html[i..]);
            break;
        };
        out.push_str(&html[i..open]);
        let rest = &lower[open..];
        let skip = if rest.starts_with("<!--") {
            Some("-->")
        } else if rest.starts_with("<script") {
            Some("</script>")
        } else if rest.starts_with("<style") {
            Some("</style>")
        } else {
            None
        };
        out.push(' ');
        i = match skip {
            Some(end) => match rest.find(end) {
                Some(p) => open + p + end.len(),
                None => break,
            },
            None => match rest.find('>') {
                Some(p) => open + p + 1,
                None => break,
            },
        };
    }
    out.replace("&nbsp;", " ").replace("&amp;", "&").replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&#39;", "'")
}

/// Adds text to `out` as it is, with the white space squashed but none added at its ends: the pieces of a word that a
/// document splits over runs of different formatting join up again. False once `out` is full.
fn push_pieces(out: &mut String, s: &str) -> bool {
    let lead = s.starts_with(char::is_whitespace);
    let trail = s.ends_with(char::is_whitespace);
    let piece = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if out.len() + piece.len() + 2 > MAX_BYTES {
        return false;
    }
    if lead && !out.is_empty() && !out.ends_with(' ') {
        out.push(' ');
    }
    out.push_str(&piece);
    if trail && !piece.is_empty() {
        out.push(' ');
    }
    true
}

/// The text a reference to a character or entity in XML stands for
fn reference(g: &quick_xml::events::BytesRef) -> Option<String> {
    match g.resolve_char_ref() {
        Ok(Some(c)) => Some(c.to_string()),
        _ => quick_xml::escape::resolve_predefined_entity(g).map(str::to_owned),
    }
}

/// The text of the elements an Office XML part holds it in (`text`: their local names), with a space after each of
/// `breaks`. Everything else, formulas and formatting included, is left out. False once `out` is full.
fn xml_text(xml: &[u8], text: &[&str], breaks: &[&str], out: &mut String) -> bool {
    let mut r = Reader::from_reader(xml);
    r.config_mut().check_end_names = false;
    let mut inside = 0usize;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let ok = match r.read_event_into(&mut buf) {
            Ok(Event::Start(e)) => {
                let name = e.local_name();
                if text.contains(&name.as_ref()) {
                    inside += 1;
                }
                true
            }
            Ok(Event::Empty(e)) => !breaks.contains(&e.local_name().as_ref()) || space(out),
            Ok(Event::End(e)) => {
                let name = e.local_name();
                if text.contains(&name.as_ref()) {
                    inside = inside.saturating_sub(1);
                }
                !breaks.contains(&name.as_ref()) || space(out)
            }
            Ok(Event::Text(t)) if inside > 0 => push_pieces(out, &t.xml10_content()),
            Ok(Event::GeneralRef(g)) if inside > 0 => reference(&g).is_none_or(|p| push_pieces(out, &p)),
            Ok(Event::Eof) | Err(_) => return true,
            _ => true,
        };
        if !ok {
            return false;
        }
    }
}

/// The parts of an archive whose names `want` accepts, in the order a document reads (slide 2 before slide 10)
fn parts<'a>(zip: &'a Archive, want: impl Fn(&str) -> bool) -> Vec<&'a Entry> {
    let mut found: Vec<_> = zip.entries.iter().filter(|e| want(&e.name)).take(MAX_PARTS).collect();
    found.sort_by(|a, b| (a.name.len(), &a.name).cmp(&(b.name.len(), &b.name)));
    found
}

/// Whether `name` is one of the parts of a Word document that hold text: the body, notes, comments, headers, footers
fn is_word_part(name: &str) -> bool {
    let Some(file) = name.strip_prefix("word/").and_then(|f| f.strip_suffix(".xml")) else {
        return false;
    };
    ["document", "footnotes", "endnotes", "comments", "header", "footer"]
        .iter()
        .any(|p| file.strip_prefix(p).is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit())))
}

fn office(kind: Kind, data: &[u8]) -> Result<String, &'static str> {
    let zip = Archive::parse(data).ok_or("It is not a document that can be read")?;
    let mut out = String::new();
    let (text, breaks, names): (&[&str], &[&str], Vec<&Entry>) = match kind {
        Kind::Docx => {
            zip.find("word/document.xml").ok_or("It is not a Word document")?;
            (&["t"], &["p", "br", "tab"], parts(&zip, is_word_part))
        }
        Kind::Xlsx => {
            zip.find("xl/workbook.xml").ok_or("It is not an Excel workbook")?;
            // Cells hold their text in the shared strings; a writer that puts it in the sheets itself comes second
            let mut names = parts(&zip, |n| n == "xl/sharedStrings.xml");
            if names.is_empty() {
                names = parts(&zip, |n| n.starts_with("xl/worksheets/") && n.ends_with(".xml"));
            }
            (&["t"], &["si", "row"], names)
        }
        Kind::Pptx => {
            zip.find("ppt/presentation.xml").ok_or("It is not a PowerPoint presentation")?;
            let slide = |n: &str| (n.starts_with("ppt/slides/slide") || n.starts_with("ppt/notesSlides/notesSlide")) && n.ends_with(".xml");
            (&["t"], &["p", "br"], parts(&zip, slide))
        }
        Kind::Odf => {
            let entry = zip.find("content.xml").ok_or("It is not an OpenDocument file")?;
            let xml = zip.read(entry, XML_CAP).ok_or("It could not be unpacked")?;
            odf_text(&xml, &mut out);
            return Ok(out);
        }
        Kind::Text | Kind::Markup => return Err("It is not a document that can be read"),
    };
    for entry in names {
        let xml = zip.read(entry, XML_CAP).ok_or("It could not be unpacked")?;
        if !xml_text(&xml, text, breaks, &mut out) {
            break;
        }
    }
    Ok(out)
}

/// OpenDocument bodies keep the text as the content of their paragraphs, headings, spans and cells
fn odf_text(xml: &[u8], out: &mut String) {
    let mut r = Reader::from_reader(xml);
    r.config_mut().check_end_names = false;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let ok = match r.read_event_into(&mut buf) {
            Ok(Event::Text(t)) => push_pieces(out, &t.xml10_content()),
            Ok(Event::GeneralRef(g)) => reference(&g).is_none_or(|p| push_pieces(out, &p)),
            Ok(Event::End(e)) if matches!(e.local_name().as_ref(), "p" | "h" | "table-cell" | "list-item") => space(out),
            Ok(Event::Eof) | Err(_) => return,
            _ => true,
        };
        if !ok {
            return;
        }
    }
}

/// The text of a file, squashed and cut to `MAX_BYTES`; the reason when there is none to read
pub fn read(kind: Kind, data: &[u8]) -> Result<String, &'static str> {
    let mut text = match kind {
        Kind::Text => {
            let mut out = String::new();
            push_words(&mut out, &decode(data).ok_or("It is not text")?);
            out
        }
        Kind::Markup => {
            let mut out = String::new();
            push_words(&mut out, &strip_markup(&decode(data).ok_or("It is not text")?));
            out
        }
        Kind::Docx | Kind::Xlsx | Kind::Pptx | Kind::Odf => office(kind, data)?,
    };
    // Cut at a character, not in the middle of one
    while text.len() > MAX_BYTES {
        text.pop();
    }
    let text = text.trim().to_string();
    if text.is_empty() { Err("It holds no text") } else { Ok(text) }
}
