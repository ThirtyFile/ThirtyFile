//! Reading the text layer of PDFs: the fonts' maps and encodings, following text along a line, page order, object
//! streams, forms, encryption, and files that are damaged or made to cost much

use std::io::Write;

use super::super::extract::{self, Kind};

fn obj(n: u32, body: &str) -> Vec<u8> {
    format!("{n} 0 obj\n{body}\nendobj\n").into_bytes()
}

fn stream(n: u32, dict: &str, data: &[u8]) -> Vec<u8> {
    let mut v = format!("{n} 0 obj\n<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    v.extend_from_slice(data);
    v.extend_from_slice(b"\nendstream\nendobj\n");
    v
}

fn flate(data: &[u8]) -> Vec<u8> {
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
    z.write_all(data).unwrap();
    z.finish().unwrap()
}

fn file(parts: &[Vec<u8>], trailer: &str) -> Vec<u8> {
    let mut v = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    for p in parts {
        v.extend_from_slice(p);
    }
    v.extend_from_slice(format!("trailer\n<< /Root 1 0 R {trailer} >>\nstartxref\n0\n%%EOF").as_bytes());
    v
}

/// A one-page file whose page shows `content` in the font `font` (the body of object 4)
pub(in crate::contentsearch) fn one_page(font: &str, content: &str) -> Vec<u8> {
    file(
        &[
            obj(1, "<< /Type /Catalog /Pages 2 0 R >>"),
            obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            obj(3, "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>"),
            obj(4, font),
            stream(5, "", content.as_bytes()),
        ],
        "",
    )
}

pub(in crate::contentsearch) const HELVETICA: &str = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>";

fn text(pdf: &[u8]) -> String {
    super::text(pdf).unwrap()
}

#[test]
fn strings_are_read_with_their_escapes_and_a_font_with_no_widths_puts_a_space_at_every_move() {
    let pdf = one_page(HELVETICA, "BT /F1 12 Tf 72 700 Td (Hello \\(world\\) caf\\351) Tj 0 -14 Td <5265706F7274> Tj T* [(Quar) 20 (terly) -300 (report)] TJ ET");
    assert_eq!(text(&pdf), "Hello (world) café Report Quarterly report");
    // Line continued with a backslash, and nested parentheses
    assert_eq!(text(&one_page(HELVETICA, "BT /F1 12 Tf (a (nested) b\\\nc) Tj ET")), "a (nested) bc");
}

#[test]
fn compressed_streams_and_a_content_stream_in_parts_are_read() {
    let content = flate(b"BT /F1 12 Tf (Compressed) Tj ET");
    let pdf = file(
        &[
            obj(1, "<< /Type /Catalog /Pages 2 0 R >>"),
            obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            obj(3, "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 4 0 R >> >> /Contents [5 0 R 6 0 R] >>"),
            obj(4, HELVETICA),
            stream(5, "/Filter /FlateDecode", &content),
            // A string that starts in one stream and ends in the next is not joined: the streams are separate tokens
            stream(6, "/Filter [/ASCIIHexDecode]", b"42 54 20 2F 46 31 20 39 20 54 66 20 28 61 6E 64 20 70 61 72 74 73 29 20 54 6A 20 45 54>"),
        ],
        "",
    );
    assert_eq!(text(&pdf), "Compressed and parts");
}

#[test]
fn a_font_of_two_byte_codes_is_read_by_its_map_and_text_placed_glyph_by_glyph_is_followed_along_the_line() {
    // What Chrome writes: a glyph at a time, each at its own place, and a glyph for the space
    let cmap = "/CIDInit /ProcSet findresource begin 12 dict begin begincmap\n1 begincodespacerange <0000> <FFFF> endcodespacerange\n\
        2 beginbfchar <0001> <0048> <0002> <0069> endbfchar\n\
        2 beginbfrange <0003> <0004> <6708> <0005> <0006> [<FB01> <0020>] endbfrange\nendcmap";
    let content = "BT /F1 10 Tf 1 0 0 1 10 100 Tm <0001> Tj 6 0 Td <0002> Tj 4 0 Td <0003> Tj 9 0 Td <0004> Tj 9 0 Td <0005> Tj\n\
        1 0 0 1 10 80 Tm <0001> Tj 6 0 Td <0002> Tj\n\
        1 0 0 1 40 80 Tm <0001> Tj ET";
    let pdf = file(
        &[
            obj(1, "<< /Type /Catalog /Pages 2 0 R >>"),
            obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            obj(3, "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>"),
            obj(4, "<< /Type /Font /Subtype /Type0 /BaseFont /X /Encoding /Identity-H /DescendantFonts [7 0 R] /ToUnicode 6 0 R >>"),
            stream(5, "", content.as_bytes()),
            stream(6, "", cmap.as_bytes()),
            // H (600) i (400), the two characters of 月 and ﬁ (900 each), a space; DW is the width of the rest
            obj(7, "<< /Type /Font /Subtype /CIDFontType2 /DW 500 /W [1 [600 400] 3 5 900] >>"),
        ],
        "",
    );
    // Each glyph is placed where the one before it ends: "Hi月有ﬁ" is one run (the ligature as its letters); on the next
    // line "Hi" again; then "H", far to the right of where that ended: a space
    assert_eq!(text(&pdf), "Hi月有fi Hi H");
}

#[test]
fn a_font_that_lists_glyph_names_for_its_codes_is_read_by_them() {
    let font = "<< /Type /Font /Subtype /Type1 /BaseFont /CMR10 /Encoding << /BaseEncoding /WinAnsiEncoding /Differences [1 /fi /eacute /uni4E2D /quoteright /fl 65 /Bsomething] >> >>";
    let pdf = one_page(font, "BT /F1 12 Tf (\\001\\002\\003\\004\\005 A) Tj ET");
    // The last glyph name isn't known: A stays what the base encoding says
    assert_eq!(text(&pdf), "fié中’fl A");
}

#[test]
fn pages_come_in_the_order_of_the_page_tree_and_inherit_their_resources() {
    let pdf = file(
        &[
            obj(1, "<< /Type /Catalog /Pages 2 0 R >>"),
            // Kids: a group holding page 2 and page 1, then page 3
            obj(2, "<< /Type /Pages /Kids [8 0 R 5 0 R] /Count 3 /Resources << /Font << /F1 4 0 R >> >> >>"),
            obj(4, HELVETICA),
            obj(5, "<< /Type /Page /Parent 2 0 R /Contents 11 0 R >>"),
            obj(8, "<< /Type /Pages /Parent 2 0 R /Kids [7 0 R 6 0 R] /Count 2 >>"),
            obj(6, "<< /Type /Page /Parent 8 0 R /Contents 10 0 R >>"),
            obj(7, "<< /Type /Page /Parent 8 0 R /Contents 9 0 R >>"),
            stream(9, "", b"BT /F1 12 Tf (first) Tj ET"),
            stream(10, "", b"BT /F1 12 Tf (second) Tj ET"),
            stream(11, "", b"BT /F1 12 Tf (third) Tj ET"),
        ],
        "",
    );
    assert_eq!(text(&pdf), "first second third");
    // A tree that loops back on itself is read once
    let looping = file(
        &[
            obj(1, "<< /Type /Catalog /Pages 2 0 R >>"),
            obj(2, "<< /Type /Pages /Kids [2 0 R 3 0 R] /Count 1 /Resources << /Font << /F1 4 0 R >> >> >>"),
            obj(3, "<< /Type /Page /Parent 2 0 R /Contents 5 0 R >>"),
            obj(4, HELVETICA),
            stream(5, "", b"BT /F1 12 Tf (once) Tj ET"),
        ],
        "",
    );
    assert_eq!(text(&looping), "once");
}

#[test]
fn objects_packed_in_an_object_stream_and_a_file_updated_after_it_was_written_are_read() {
    // Objects 2 (the pages), 3 (a page) and 4 (the font) are inside object stream 6
    let (pages, page, font) =
        ("<< /Type /Pages /Kids [3 0 R] /Count 1 >>", "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>", HELVETICA);
    let header = format!("2 0 3 {} 4 {}", pages.len() + 1, pages.len() + page.len() + 2);
    let body = format!("{header}\n{pages}\n{page}\n{font}");
    let packed = flate(body.as_bytes());
    let mut parts = vec![
        obj(1, "<< /Type /Catalog /Pages 2 0 R >>"),
        stream(6, &format!("/Type /ObjStm /N 3 /First {} /Filter /FlateDecode", header.len() + 1), &packed),
        stream(5, "", b"BT /F1 12 Tf (old text) Tj ET"),
    ];
    assert_eq!(text(&file(&parts, "")), "old text");
    // An update writes the content again further on: the later one is the page's
    parts.push(stream(5, "", b"BT /F1 12 Tf (new text) Tj ET"));
    assert_eq!(text(&file(&parts, "")), "new text");
}

#[test]
fn forms_are_read_where_they_are_drawn_and_a_form_that_draws_itself_is_read_once() {
    let pdf = file(
        &[
            obj(1, "<< /Type /Catalog /Pages 2 0 R >>"),
            obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            obj(3, "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 4 0 R >> /XObject << /X1 6 0 R >> >> /Contents 5 0 R >>"),
            obj(4, HELVETICA),
            stream(5, "", b"BT /F1 12 Tf (before) Tj ET /X1 Do BT /F1 12 Tf (after) Tj ET"),
            // The form has resources of its own, and draws itself and the page's other form
            stream(
                6,
                "/Type /XObject /Subtype /Form /Resources << /Font << /F1 4 0 R >> /XObject << /X1 6 0 R /X2 7 0 R >> >>",
                b"BT /F1 12 Tf (inside) Tj ET /X1 Do /X2 Do",
            ),
            stream(7, "/Type /XObject /Subtype /Form", b"BT /F1 12 Tf (deeper) Tj ET"),
        ],
        "",
    );
    assert_eq!(text(&pdf), "before inside deeper after");
}

#[test]
fn files_that_cannot_be_read_say_why_and_nothing_panics_on_damaged_ones() {
    assert_eq!(super::text(b"just some text").unwrap_err(), "It is not a PDF");
    assert_eq!(super::text(b"").unwrap_err(), "It is not a PDF");
    assert_eq!(super::text(b"%PDF-1.4\n").unwrap_err(), "It holds no pages");
    // Encrypted: the text can't be read without the key
    let encrypted = file(&[obj(1, "<< /Type /Catalog >>")], "/Encrypt 9 0 R");
    assert_eq!(super::text(&encrypted).unwrap_err(), "It is encrypted");
    // A page that is a picture holds no text
    let scan = one_page(HELVETICA, "q 600 0 0 800 0 0 cm /Im0 Do Q BI /W 1 /H 1 /CS /G /BPC 8 ID \u{1}\u{2} EI");
    assert_eq!(extract::read(Kind::Pdf, &scan).unwrap_err(), "It holds no text");
    // Cut off anywhere, or changed anywhere: no panic, whatever comes out
    let good = one_page(HELVETICA, "BT /F1 12 Tf (Some text) Tj ET");
    for end in 0..good.len() {
        let _ = super::text(&good[..end]);
    }
    let mut seed = 12345u64;
    for _ in 0..3000 {
        let mut copy = good.clone();
        for _ in 0..4 {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let at = (seed >> 33) as usize % copy.len();
            copy[at] = (seed >> 20) as u8;
        }
        let _ = super::text(&copy);
    }
}

#[test]
fn a_stream_that_expands_without_end_costs_a_bounded_amount() {
    // 100 MB of nothing in a few KB
    let zeros = flate(&vec![0u8; 100 * 1024 * 1024]);
    assert!(zeros.len() < 2 * 1024 * 1024, "{}", zeros.len());
    let pdf = file(
        &[
            obj(1, "<< /Type /Catalog /Pages 2 0 R >>"),
            obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            obj(3, "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>"),
            obj(4, HELVETICA),
            stream(5, "/Filter /FlateDecode", &zeros),
        ],
        "",
    );
    let started = std::time::Instant::now();
    assert_eq!(extract::read(Kind::Pdf, &pdf).unwrap_err(), "It holds no text");
    assert!(started.elapsed() < std::time::Duration::from_secs(20), "{:?}", started.elapsed());
    // Text that never ends is cut at the limit
    let line = "BT /F1 12 Tf (abcdefghij klmnopqrst) Tj ET\n".repeat(200_000);
    let long = one_page(HELVETICA, &line);
    let kept = extract::read(Kind::Pdf, &long).unwrap();
    assert!(kept.len() <= extract::MAX_BYTES && kept.len() > extract::MAX_BYTES - 100, "{}", kept.len());
}

#[test]
fn text_is_kept_in_compatibility_form_so_what_chrome_writes_as_radicals_is_found() {
    // A font whose map gives the radical ⽉ (U+2F49) for 月, and full-width letters
    let cmap = "1 begincodespacerange <00> <FF> endcodespacerange 3 beginbfchar <01> <2F49> <02> <0041> <03> <0020> <04> <FF22> endbfchar";
    let pdf = file(
        &[
            obj(1, "<< /Type /Catalog /Pages 2 0 R >>"),
            obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            obj(3, "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>"),
            obj(4, "<< /Type /Font /Subtype /TrueType /ToUnicode 6 0 R >>"),
            stream(5, "", b"BT /F1 12 Tf (\\001\\001\\003\\002\\004) Tj ET"),
            stream(6, "", cmap.as_bytes()),
        ],
        "",
    );
    assert_eq!(extract::read(Kind::Pdf, &pdf).unwrap(), "月月 AB");
}

#[test]
fn a_font_with_widths_is_followed_along_the_line_and_a_break_between_chinese_characters_is_no_space() {
    // Widths from code 72 ("H") on: H is 700, I is 300 (thousandths of the size, 10)
    let font = "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /FirstChar 72 /LastChar 73 /Widths [700 300] >>";
    let pdf = one_page(font, "BT /F1 10 Tf 10 100 Td (H) Tj 7 0 Td (I) Tj 3 0 Td (H) Tj 12 0 Td (I) Tj ET");
    // Each starts where the one before ended, except the last, 5 units (half the size) further on
    assert_eq!(text(&pdf), "HIH I");

    // A line that breaks in the middle of Chinese words joins them; between Chinese and Latin letters it keeps its space
    let cmap = "1 begincodespacerange <00> <FF> endcodespacerange 3 beginbfchar <01> <5B63> <02> <5EA6> <03> <0041> endbfchar";
    let pdf = file(
        &[
            obj(1, "<< /Type /Catalog /Pages 2 0 R >>"),
            obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            obj(3, "<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>"),
            obj(4, "<< /Type /Font /Subtype /TrueType /ToUnicode 6 0 R >>"),
            stream(5, "", b"BT /F1 12 Tf 0 14 TD (\\001) Tj T* (\\002) Tj T* (\\003) Tj T* (\\001) Tj ET"),
            stream(6, "", cmap.as_bytes()),
        ],
        "",
    );
    assert_eq!(text(&pdf), "季度 A 季");
}
