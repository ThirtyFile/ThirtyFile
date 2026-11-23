//! Page operations on PDFs: classic files, files with object streams and cross-reference streams, pages that inherit
//! their attributes, files that were updated in place, and files that can't be used. Every file made is checked for
//! its structure: a cross-reference table that points at the objects, a page tree that counts its pages, references
//! that lead somewhere.

use std::io::Write;

use super::{Opened, PdfError, Pick, arrange, combine, object::Obj, open, parse_ranges};

pub(super) fn obj(n: u32, body: &str) -> Vec<u8> {
    format!("{n} 0 obj\n{body}\nendobj\n").into_bytes()
}

pub(super) fn stream(n: u32, dict: &str, data: &[u8]) -> Vec<u8> {
    let mut v = format!("{n} 0 obj\n<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    v.extend_from_slice(data);
    v.extend_from_slice(b"\nendstream\nendobj\n");
    v
}

pub(super) fn flate(data: &[u8]) -> Vec<u8> {
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    z.write_all(data).unwrap();
    z.finish().unwrap()
}

pub(super) fn classic(parts: &[Vec<u8>], trailer: &str) -> Vec<u8> {
    let mut v = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    for p in parts {
        v.extend_from_slice(p);
    }
    v.extend_from_slice(format!("trailer\n<< /Root 1 0 R {trailer} >>\nstartxref\n0\n%%EOF").as_bytes());
    v
}

/// The content stream of a page that says `label`
pub(super) fn content(n: u32, label: &str) -> Vec<u8> {
    stream(n, "", format!("BT /F1 12 Tf 72 700 Td ({label}) Tj ET").as_bytes())
}

/// A file of pages, whose labels are "A", "B", "C": the media box, the resources and the rotation are inherited from the
/// page tree, except that page B has a media box of its own and page C a rotation
pub(super) fn three_pages() -> Vec<u8> {
    classic(
        &[
            obj(1, "<< /Type /Catalog /Pages 2 0 R >>"),
            obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R 5 0 R] /Count 3 /MediaBox [0 0 612 792] /Rotate 90 /Resources << /Font << /F1 9 0 R >> >> >>"),
            obj(3, "<< /Type /Page /Parent 2 0 R /Contents 6 0 R >>"),
            obj(4, "<< /Type /Page /Parent 2 0 R /Contents 7 0 R /MediaBox [0 0 200 300] >>"),
            obj(5, "<< /Type /Page /Parent 2 0 R /Contents 8 0 R /Rotate 270 >>"),
            content(6, "A"),
            content(7, "B"),
            content(8, "C"),
            obj(9, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        ],
        "",
    )
}

/// What each page of a file says, in order
pub(super) fn labels(data: &[u8]) -> Vec<String> {
    let opened = open(data).unwrap();
    opened.pages.iter().map(|p| label(&opened, p)).collect()
}

fn label(opened: &Opened, page: &super::Page) -> String {
    let bytes = opened.doc.stream(page.dict.get("Contents").unwrap()).unwrap();
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let start = text.find('(').unwrap() + 1;
    text[start..text.find(')').unwrap()].to_string()
}

fn number(opened: &Opened, page: usize, key: &str) -> Option<i64> {
    opened.pages[page].dict.get(key).and_then(|o| opened.doc.resolve(o).int())
}

fn pick(page: usize, rotate: i32) -> Pick {
    Pick { page, rotate }
}

/// The file is a PDF as a reader expects it: the header, a cross-reference table whose every entry points at its object,
/// a trailer that names the catalog, a page tree that counts its pages, and no reference to an object that isn't there
pub(super) fn check_structure(data: &[u8]) {
    assert!(data.starts_with(b"%PDF-1."));
    assert!(data.ends_with(b"%%EOF\n"));
    let text = String::from_utf8_lossy(data).into_owned();
    // startxref points at "xref"
    let start: usize = text.rsplit("startxref\n").next().unwrap().lines().next().unwrap().parse().unwrap();
    assert!(data[start..].starts_with(b"xref\n"), "startxref doesn't point at the table");
    let table = String::from_utf8_lossy(&data[start..]).into_owned();
    let mut lines = table.lines().skip(1);
    let (first, size) = lines.next().unwrap().split_once(' ').unwrap();
    assert_eq!(first, "0");
    let size: usize = size.parse().unwrap();
    assert!(lines.next().unwrap().ends_with(" 65535 f "));
    for number in 1..size {
        let entry = lines.next().unwrap();
        assert_eq!(entry.len(), 19, "{entry:?}");
        assert!(entry.ends_with(" 00000 n "));
        let offset: usize = entry[..10].parse().unwrap();
        assert!(data[offset..].starts_with(format!("{number} 0 obj\n").as_bytes()), "object {number} is not at {offset}");
    }
    assert!(lines.next().unwrap().starts_with("trailer"));
    assert!(lines.next().unwrap().contains(&format!("/Size {size} /Root 1 0 R")));
    // The objects read back, the tree counts its pages, and every reference is to an object
    let opened = open(data).unwrap();
    let Obj::Dict(catalog) = opened.doc.object(1) else { panic!("no catalog") };
    assert_eq!(catalog.get("Type").and_then(Obj::name), Some("Catalog"));
    let Obj::Dict(tree) = opened.doc.object(2) else { panic!("no page tree") };
    let Some(Obj::Array(kids)) = tree.get("Kids") else { panic!("no kids") };
    assert_eq!(tree.get("Count").and_then(Obj::int), Some(kids.len() as i64));
    assert_eq!(kids.len(), opened.pages.len());
    for number in opened.doc.numbers() {
        assert!(number < size as u32);
        refs_lead_somewhere(&opened, &opened.doc.object(number));
    }
    for page in &opened.pages {
        assert_eq!(page.dict.get("Parent"), Some(&Obj::Ref(2)));
        assert!(page.dict.contains_key("MediaBox"), "a page keeps its media box");
        assert!(page.dict.contains_key("Resources"), "a page keeps its resources");
    }
}

fn refs_lead_somewhere(opened: &Opened, obj: &Obj) {
    match obj {
        Obj::Ref(n) => assert_ne!(opened.doc.object(*n), Obj::Null, "reference to object {n}, which is not there"),
        Obj::Array(items) => items.iter().for_each(|i| refs_lead_somewhere(opened, i)),
        Obj::Dict(d) | Obj::Stream(d, _) => d.values().for_each(|v| refs_lead_somewhere(opened, v)),
        _ => {}
    }
}

#[test]
fn pages_are_kept_in_the_order_chosen_and_the_others_left_out() {
    let source = three_pages();
    let opened = open(&source).unwrap();
    let out = arrange(&opened, &[pick(2, 0), pick(0, 0)]).unwrap();
    check_structure(&out);
    assert_eq!(labels(&out), ["C", "A"]);
    // Page B's stream is not in the file: only what the pages use is copied
    assert!(!String::from_utf8_lossy(&out).contains("(B)"));
    // The whole file, in another order
    let out = arrange(&opened, &[pick(1, 0), pick(2, 0), pick(0, 0)]).unwrap();
    check_structure(&out);
    assert_eq!(labels(&out), ["B", "C", "A"]);
}

#[test]
fn pages_keep_what_they_inherited_and_turning_adds_to_the_rotation() {
    let source = three_pages();
    let opened = open(&source).unwrap();
    let out = arrange(&opened, &[pick(0, 90), pick(1, -90), pick(2, 180), pick(0, 0)]).unwrap();
    check_structure(&out);
    let result = open(&out).unwrap();
    // A inherits 90 and is turned 90 more; B inherits 90 and is turned back to 0; C had 270, plus 180 is 90
    assert_eq!([0, 1, 2, 3].map(|i| number(&result, i, "Rotate").unwrap()), [180, 0, 90, 90]);
    // Page B's own media box is kept, the others have the inherited one
    let media = |i: usize| match &result.pages[i].dict["MediaBox"] {
        Obj::Array(a) => a.iter().filter_map(Obj::int).collect::<Vec<_>>(),
        other => panic!("{other:?}"),
    };
    assert_eq!(media(0), [0, 0, 612, 792]);
    assert_eq!(media(1), [0, 0, 200, 300]);
    // The font that resources named is in the file
    let Obj::Dict(resources) = result.doc.resolve(&result.pages[0].dict["Resources"]) else { panic!() };
    assert!(matches!(result.doc.resolve(&resources["Font"]), Obj::Dict(_)));
    // Not turned: the rotation of the source stays
    let same = arrange(&opened, &[pick(2, 0)]).unwrap();
    assert_eq!(number(&open(&same).unwrap(), 0, "Rotate"), Some(270));
}

#[test]
fn a_file_with_object_streams_and_a_cross_reference_stream_is_read_and_written() {
    // Catalog, page tree and both pages live in an object stream (6); the table is a stream (9)
    let members = [
        (1, "<< /Type /Catalog /Pages 2 0 R >>"),
        (2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 300 300] /Resources << /Font << /F1 5 0 R >> >> >>"),
        (3, "<< /Type /Page /Parent 2 0 R /Contents 7 0 R >>"),
        (4, "<< /Type /Page /Parent 2 0 R /Contents 8 0 R >>"),
        (5, "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
    ];
    let mut header = String::new();
    let mut body = String::new();
    for (n, text) in members {
        header.push_str(&format!("{n} {} ", body.len()));
        body.push_str(text);
        body.push('\n');
    }
    let packed = flate(format!("{header}{body}").as_bytes());
    let mut file = b"%PDF-1.5\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut at = Vec::new();
    for (n, part) in
        [(6, stream(6, &format!("/Type /ObjStm /N 5 /First {} /Filter /FlateDecode", header.len()), &packed)), (7, content(7, "one")), (8, content(8, "two"))]
    {
        at.push((n, file.len()));
        file.extend_from_slice(&part);
    }
    // Entries: type, field 2, field 3 (one byte, two bytes, one byte)
    let mut table = vec![0u8, 0, 0, 255];
    for n in 1..=5u32 {
        table.extend_from_slice(&[2, 0, 6, (n - 1) as u8]);
    }
    for (_, offset) in &at {
        table.extend_from_slice(&[1, (*offset >> 8) as u8, *offset as u8, 0]);
    }
    let xref_at = file.len();
    table.extend_from_slice(&[1, (xref_at >> 8) as u8, xref_at as u8, 0]);
    file.extend_from_slice(&stream(9, "/Type /XRef /Size 10 /W [1 2 1] /Root 1 0 R", &table));
    file.extend_from_slice(format!("startxref\n{xref_at}\n%%EOF\n").as_bytes());

    let opened = open(&file).unwrap();
    assert_eq!(opened.pages.len(), 2);
    let out = arrange(&opened, &[pick(1, 0), pick(0, 90)]).unwrap();
    check_structure(&out);
    assert_eq!(labels(&out), ["two", "one"]);
    let result = open(&out).unwrap();
    assert_eq!(number(&result, 1, "Rotate"), Some(90));
    // The object stream and the table of the source are not in the new file
    assert!(!String::from_utf8_lossy(&out).contains("ObjStm") && !String::from_utf8_lossy(&out).contains("/XRef"));
}

#[test]
fn a_file_that_was_updated_in_place_gives_the_latest_of_each_object() {
    let mut file = three_pages();
    // An update: page B's content is replaced, and a page tree of two pages (A and B) is written again
    file.push(b'\n');
    file.extend_from_slice(&content(7, "B2"));
    file.extend_from_slice(&obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 612 792] /Resources << /Font << /F1 9 0 R >> >> >>"));
    file.extend_from_slice(b"trailer\n<< /Root 1 0 R /Prev 0 >>\nstartxref\n0\n%%EOF\n");
    let opened = open(&file).unwrap();
    assert_eq!(opened.pages.len(), 2);
    let out = arrange(&opened, &[pick(1, 0), pick(0, 0)]).unwrap();
    check_structure(&out);
    assert_eq!(labels(&out), ["B2", "A"]);
    assert!(!String::from_utf8_lossy(&out).contains("(B)"));
}

#[test]
fn streams_are_copied_as_they_are_and_links_to_pages_follow_or_end() {
    let data: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
    let compressed = flate(&data);
    let file = classic(
        &[
            obj(1, "<< /Type /Catalog /Pages 2 0 R >>"),
            obj(2, "<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 /MediaBox [0 0 100 100] /Resources << /XObject << /Im1 6 0 R >> >> >>"),
            // Page A has a link to page B and an annotation that names its own page
            obj(3, "<< /Type /Page /Parent 2 0 R /Contents 5 0 R /Annots [7 0 R] >>"),
            obj(4, "<< /Type /Page /Parent 2 0 R /Contents 5 0 R >>"),
            stream(5, "", b"q Q"),
            stream(6, "/Type /XObject /Subtype /Image /Width 5000 /Height 1 /BitsPerComponent 8 /ColorSpace /DeviceGray /Filter /FlateDecode", &compressed),
            obj(7, "<< /Type /Annot /Subtype /Link /P 3 0 R /Rect [0 0 10 10] /Dest [4 0 R /Fit] /Name /A#20B >>"),
        ],
        "",
    );
    let opened = open(&file).unwrap();
    // Both pages: the link leads to the new page B
    let out = arrange(&opened, &[pick(0, 0), pick(1, 0)]).unwrap();
    check_structure(&out);
    let result = open(&out).unwrap();
    let Obj::Array(annots) = result.doc.resolve(&result.pages[0].dict["Annots"]) else { panic!() };
    let Obj::Dict(annot) = result.doc.resolve(&annots[0]) else { panic!() };
    let Obj::Array(dest) = &annot["Dest"] else { panic!() };
    let Obj::Ref(target) = dest[0] else { panic!("{dest:?}") };
    assert_eq!(result.doc.object(target), result.doc.resolve(&Obj::Ref(*result.pages[1].number.as_ref().unwrap())));
    assert_eq!(annot["P"], Obj::Ref(result.pages[0].number.unwrap()));
    assert_eq!(annot["Name"], Obj::Name("A B".into()));
    // The picture is byte for byte what it was
    let Obj::Dict(resources) = result.doc.resolve(&result.pages[0].dict["Resources"]) else { panic!() };
    let Obj::Dict(images) = result.doc.resolve(&resources["XObject"]) else { panic!() };
    let Obj::Stream(_, range) = result.doc.resolve(&images["Im1"]) else { panic!() };
    assert_eq!(&out[range], compressed.as_slice());
    // Only page A: the link to page B is gone, and so is page B
    let out = arrange(&opened, &[pick(0, 0)]).unwrap();
    check_structure(&out);
    let result = open(&out).unwrap();
    assert_eq!(result.pages.len(), 1);
    let Obj::Array(annots) = result.doc.resolve(&result.pages[0].dict["Annots"]) else { panic!() };
    let Obj::Dict(annot) = result.doc.resolve(&annots[0]) else { panic!() };
    assert_eq!(annot["Dest"], Obj::Array(vec![Obj::Null, Obj::Name("Fit".into())]));
}

#[test]
fn files_are_combined_page_after_page_and_every_file_keeps_its_own_objects() {
    let a = three_pages();
    // A second file whose objects have the same numbers
    let b = classic(
        &[
            obj(1, "<< /Type /Catalog /Pages 2 0 R >>"),
            obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 /MediaBox [0 0 50 50] /Resources << /Font << /F1 9 0 R >> >> >>"),
            obj(3, "<< /Type /Page /Parent 2 0 R /Contents 6 0 R >>"),
            content(6, "X"),
            obj(9, "<< /Type /Font /Subtype /Type1 /BaseFont /Courier >>"),
        ],
        "",
    );
    let (oa, ob) = (open(&a).unwrap(), open(&b).unwrap());
    let out = combine(&[ob, oa]).unwrap();
    check_structure(&out);
    assert_eq!(labels(&out), ["X", "A", "B", "C"]);
    let result = open(&out).unwrap();
    let font = |i: usize| {
        let Obj::Dict(r) = result.doc.resolve(&result.pages[i].dict["Resources"]) else { panic!() };
        let Obj::Dict(f) = result.doc.resolve(&r["Font"]) else { panic!() };
        let Obj::Dict(f1) = result.doc.resolve(&f["F1"]) else { panic!() };
        f1["BaseFont"].clone()
    };
    assert_eq!((font(0), font(1)), (Obj::Name("Courier".into()), Obj::Name("Helvetica".into())));
}

#[test]
fn a_file_is_combined_with_itself_and_pages_can_be_taken_twice() {
    let source = three_pages();
    let out = combine(&[open(&source).unwrap(), open(&source).unwrap()]).unwrap();
    check_structure(&out);
    assert_eq!(labels(&out), ["A", "B", "C", "A", "B", "C"]);
    let out = arrange(&open(&source).unwrap(), &[pick(0, 0), pick(0, 90)]).unwrap();
    check_structure(&out);
    assert_eq!(labels(&out), ["A", "A"]);
    let result = open(&out).unwrap();
    assert_eq!((number(&result, 0, "Rotate"), number(&result, 1, "Rotate")), (Some(90), Some(180)));
}

#[test]
fn a_page_in_a_file_with_no_page_tree_is_found_and_used() {
    let file =
        classic(&[obj(1, "<< /Type /Catalog >>"), obj(3, "<< /Type /Page /MediaBox [0 0 10 10] /Resources << >> /Contents 6 0 R >>"), content(6, "lost")], "");
    let out = arrange(&open(&file).unwrap(), &[pick(0, 0)]).unwrap();
    check_structure(&out);
    assert_eq!(labels(&out), ["lost"]);
}

#[test]
fn what_is_not_usable_is_refused_with_a_reason() {
    assert_eq!(open(b"hello").err(), Some(PdfError::NotPdf));
    assert_eq!(open(b"").err(), Some(PdfError::NotPdf));
    // Encrypted
    let encrypted = classic(&[obj(1, "<< /Type /Catalog /Pages 2 0 R >>"), obj(2, "<< /Type /Pages /Kids [] /Count 0 >>")], "/Encrypt << /Filter /Standard >>");
    assert_eq!(open(&encrypted).err(), Some(PdfError::Encrypted));
    assert!(PdfError::Encrypted.message().contains("encrypted"));
    // Damaged: cut off, nothing that is a page
    let cut = &three_pages()[..40];
    assert_eq!(open(cut).err(), Some(PdfError::NoPages));
    // A page tree that loops on itself doesn't hang
    let looped = classic(&[obj(1, "<< /Type /Catalog /Pages 2 0 R >>"), obj(2, "<< /Type /Pages /Kids [2 0 R] /Count 1 >>")], "");
    assert_eq!(open(&looped).err(), Some(PdfError::NoPages));
    // Bad choices
    let source = three_pages();
    let opened = open(&source).unwrap();
    assert!(arrange(&opened, &[]).is_err());
    assert!(arrange(&opened, &[pick(3, 0)]).is_err());
    assert!(arrange(&opened, &[pick(0, 45)]).is_err());
    assert!(combine(&[]).is_err());
}

#[test]
fn page_ranges_are_read_as_people_write_them() {
    assert_eq!(parse_ranges("1-3, 5", 8), Ok(vec![(0, 2), (4, 4)]));
    assert_eq!(parse_ranges(" 2 ; 4- ", 6), Ok(vec![(1, 1), (3, 5)]));
    assert_eq!(parse_ranges("-2", 6), Ok(vec![(0, 1)]));
    assert_eq!(parse_ranges("3,1", 3), Ok(vec![(2, 2), (0, 0)]));
    for bad in ["", "0", "3-1", "a", "1-9", "9", "1--2"] {
        assert!(parse_ranges(bad, 8).is_err(), "{bad:?}");
    }
    assert!(parse_ranges("1-9", 8).unwrap_err().contains("8 pages"));
}
