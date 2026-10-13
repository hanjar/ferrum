//! FER-493: Office text extraction on small packages built in the test (zip + minimal XML).

use super::*;
use std::io::{Cursor, Write};

const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main" xmlns:mc="http://schemas.openxmlformats.org/markup-compatibility/2006""#;

fn package(parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (name, data) in parts {
        w.start_file(*name, opts).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

fn run(bytes: Vec<u8>, format: Format) -> Res<Extraction> {
    extract_from(Cursor::new(bytes), format, &Limits::default())
}

fn run_with(bytes: Vec<u8>, format: Format, limits: Limits) -> Res<Extraction> {
    extract_from(Cursor::new(bytes), format, &limits)
}

fn document(body: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {W}><w:body>{body}</w:body></w:document>"#
    )
}

fn p(text: &str) -> String {
    format!("<w:p><w:r><w:t>{text}</w:t></w:r></w:p>")
}

fn docx(body: &str) -> Vec<u8> {
    package(&[("word/document.xml", document(body).as_bytes())])
}

const STYLES: &str = r#"<w:styles xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:latentStyles><w:lsdException w:name="heading 1"/></w:latentStyles>
<w:style w:type="paragraph" w:styleId="Heading1"><w:name w:val="heading 1"/></w:style>
<w:style w:type="paragraph" w:styleId="Judul2"><w:name w:val="heading 2"/></w:style>
<w:style w:type="paragraph" w:styleId="Normal"><w:name w:val="Normal"/></w:style>
</w:styles>"#;

const NUMBERING: &str = r#"<w:numbering xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">
<w:abstractNum w:abstractNumId="0"><w:lvl w:ilvl="0"><w:numFmt w:val="bullet"/></w:lvl><w:lvl w:ilvl="1"><w:numFmt w:val="bullet"/></w:lvl></w:abstractNum>
<w:abstractNum w:abstractNumId="1"><w:lvl w:ilvl="0"><w:numFmt w:val="decimal"/></w:lvl></w:abstractNum>
<w:num w:numId="1"><w:abstractNumId w:val="0"/></w:num>
<w:num w:numId="2"><w:abstractNumId w:val="1"/></w:num>
</w:numbering>"#;

fn styled(style: &str, text: &str) -> String {
    format!(r#"<w:p><w:pPr><w:pStyle w:val="{style}"/></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#)
}

fn listed(num: u32, ilvl: u32, text: &str) -> String {
    format!(
        r#"<w:p><w:pPr><w:pStyle w:val="ListParagraph"/><w:numPr><w:ilvl w:val="{ilvl}"/><w:numId w:val="{num}"/></w:numPr><w:tabs><w:tab w:val="left" w:pos="720"/></w:tabs></w:pPr><w:r><w:t>{text}</w:t></w:r></w:p>"#
    )
}

#[test]
fn format_from_name() {
    assert_eq!(format_by_name("a.DOCX"), Some(Format::Docx));
    assert_eq!(format_by_name("macro.docm"), Some(Format::Docx));
    assert_eq!(format_by_name("b.xlsm"), Some(Format::Xlsx));
    assert_eq!(format_by_name("c.ppsx"), Some(Format::Pptx));
    assert_eq!(format_by_name("d.odt"), Some(Format::Odt));
    assert_eq!(format_by_name("old.doc"), None);
    assert_eq!(format_by_name("noext"), None);
}

#[test]
fn docx_headings_lists_tables_and_side_parts() {
    let body = [
        styled("Heading1", "Getting Started"),
        p("Hello &amp; welcome&#33;"),
        listed(1, 0, "first bullet"),
        listed(1, 1, "nested bullet"),
        listed(2, 0, "step one"),
        listed(2, 0, "step two"),
        styled("Judul2", "Install"),
        r#"<w:p><w:r><w:t>a</w:t><w:tab/><w:t>b</w:t><w:br/><w:t xml:space="preserve">c </w:t></w:r></w:p>"#.to_owned(),
        r#"<w:tbl><w:tr><w:tc><w:p><w:r><w:t>Key</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Action</w:t></w:r></w:p></w:tc></w:tr><w:tr><w:tc><w:p><w:r><w:t>F5</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>Copy</w:t></w:r></w:p><w:p><w:r><w:t>files</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#.to_owned(),
        r#"<w:p><w:r><mc:AlternateContent><mc:Choice Requires="wps"><w:txbxContent><w:p><w:r><w:t>Box text</w:t></w:r></w:p></w:txbxContent></mc:Choice><mc:Fallback><w:txbxContent><w:p><w:r><w:t>Box text</w:t></w:r></w:p></w:txbxContent></mc:Fallback></mc:AlternateContent></w:r></w:p>"#.to_owned(),
        r#"<w:p><w:r><w:delText>deleted</w:delText><w:instrText>PAGE</w:instrText></w:r></w:p>"#.to_owned(),
    ]
    .concat();
    let header = format!("<w:hdr {W}>{}</w:hdr>", p("Acme Corp"));
    let footnotes = format!(
        r#"<w:footnotes {W}><w:footnote w:type="separator" w:id="-1"><w:p><w:r><w:separator/></w:r></w:p></w:footnote><w:footnote w:id="1">{}</w:footnote></w:footnotes>"#,
        p("A footnote")
    );
    let bytes = package(&[
        ("word/document.xml", document(&body).as_bytes()),
        ("word/styles.xml", STYLES.as_bytes()),
        ("word/numbering.xml", NUMBERING.as_bytes()),
        ("word/header1.xml", header.as_bytes()),
        ("word/header2.xml", header.as_bytes()),
        ("word/footnotes.xml", footnotes.as_bytes()),
    ]);
    let out = run(bytes, Format::Docx).unwrap();
    let t = out.text;
    assert!(!out.truncated);
    assert!(t.starts_with("# Getting Started\n"), "{t}");
    assert!(t.contains("Hello & welcome!\n"), "{t}");
    assert!(t.contains("- first bullet\n  - nested bullet\n"), "{t}");
    assert!(t.contains("1. step one\n2. step two\n"), "{t}");
    assert!(t.contains("\n## Install\n"), "{t}");
    assert!(t.contains("a\tb\nc\n"), "{t}");
    assert!(t.contains("| Key | Action |\n| F5 | Copy files |\n"), "{t}");
    assert_eq!(t.matches("Box text").count(), 1, "{t}");
    assert!(!t.contains("deleted") && !t.contains("PAGE"), "{t}");
    assert_eq!(t.matches("Acme Corp").count(), 1, "{t}");
    assert!(t.contains("## Header\nAcme Corp\n"), "{t}");
    assert!(t.contains("## Catatan kaki\nA footnote\n"), "{t}");
}

#[test]
fn docx_heading_by_style_id_without_styles_part() {
    let t = run(docx(&styled("Heading3", "Deep")), Format::Docx)
        .unwrap()
        .text;
    assert_eq!(t, "### Deep\n");
}

#[test]
fn docx_nested_table_goes_into_parent_cell() {
    let inner = r#"<w:tbl><w:tr><w:tc><w:p><w:r><w:t>x</w:t></w:r></w:p></w:tc><w:tc><w:p><w:r><w:t>y</w:t></w:r></w:p></w:tc></w:tr></w:tbl>"#;
    let outer = format!(
        r#"<w:tbl><w:tr><w:tc><w:p><w:r><w:t>outer</w:t></w:r></w:p>{inner}</w:tc></w:tr></w:tbl>"#
    );
    let t = run(docx(&outer), Format::Docx).unwrap().text;
    assert_eq!(t, "| outer | x | y | |\n");
}

#[test]
fn docx_without_text_is_empty() {
    let out = run(docx("<w:p/><w:sectPr/>"), Format::Docx).unwrap();
    assert_eq!(out.text, "");
    assert!(!out.truncated);
}

#[test]
fn docx_without_document_part_is_corrupt() {
    let bytes = package(&[("word/styles.xml", STYLES.as_bytes())]);
    assert_eq!(run(bytes, Format::Docx), Err(OfficeError::Corrupt));
}

#[test]
fn doctype_and_entities_are_refused() {
    // Billion-laughs style DTD: refused before any entity is looked at.
    let xml = format!(
        r#"<?xml version="1.0"?><!DOCTYPE w:document [<!ENTITY a "AAAAAAAAAA"><!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;"><!ENTITY xxe SYSTEM "file:///C:/Windows/win.ini">]><w:document {W}><w:body><w:p><w:r><w:t>&b;&xxe;</w:t></w:r></w:p></w:body></w:document>"#
    );
    let bytes = package(&[("word/document.xml", xml.as_bytes())]);
    assert_eq!(run(bytes, Format::Docx), Err(OfficeError::Doctype));
}

#[test]
fn unknown_entities_are_dropped_not_expanded() {
    let t = run(docx(&p("x&ext;y &#65;&lt;&#x42;")), Format::Docx)
        .unwrap()
        .text;
    assert_eq!(t, "xy A<B\n");
}

#[test]
fn deep_xml_is_refused() {
    let open = "<w:sdt>".repeat(300);
    let close = "</w:sdt>".repeat(300);
    let bytes = docx(&format!("{open}{close}"));
    assert_eq!(
        run(bytes, Format::Docx),
        Err(OfficeError::Limit("kedalaman XML"))
    );
}

#[test]
fn malformed_xml_is_corrupt() {
    let bytes = package(&[(
        "word/document.xml",
        b"<w:document><w:body></w:p>".as_slice(),
    )]);
    assert_eq!(run(bytes, Format::Docx), Err(OfficeError::Corrupt));
}

#[test]
fn zip_bomb_ratio_is_refused() {
    // 3 MB of the same byte deflates ~1000:1.
    let filler = " ".repeat(3 * 1024 * 1024);
    let bytes = docx(&format!("<w:p><w:r><w:t>{filler}</w:t></w:r></w:p>"));
    assert!(bytes.len() < 64 * 1024);
    assert_eq!(
        run(bytes, Format::Docx),
        Err(OfficeError::Limit("rasio kompresi"))
    );
}

#[test]
fn total_inflated_budget_is_enforced() {
    let limits = Limits {
        max_inflated: 4 * 1024,
        ..Limits::default()
    };
    let body: String = (0..400).map(|i| p(&format!("paragraph {i}"))).collect();
    assert_eq!(
        run_with(docx(&body), Format::Docx, limits),
        Err(OfficeError::Limit("total data terurai"))
    );
}

#[test]
fn too_many_entries_are_refused() {
    let limits = Limits {
        max_entries: 3,
        ..Limits::default()
    };
    let doc = document(&p("x"));
    let bytes = package(&[
        ("word/document.xml", doc.as_bytes()),
        ("a", b"1".as_slice()),
        ("b", b"2".as_slice()),
        ("c", b"3".as_slice()),
    ]);
    assert_eq!(
        run_with(bytes, Format::Docx, limits),
        Err(OfficeError::Limit("jumlah entri zip"))
    );
}

#[test]
fn encrypted_member_is_refused() {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let plain = zip::write::SimpleFileOptions::default();
    w.start_file("[Content_Types].xml", plain).unwrap();
    w.write_all(b"<Types/>").unwrap();
    let secret = zip::write::SimpleFileOptions::default()
        .with_aes_encryption(zip::AesMode::Aes256, "password");
    w.start_file("word/document.xml", secret).unwrap();
    w.write_all(document(&p("secret")).as_bytes()).unwrap();
    let bytes = w.finish().unwrap().into_inner();
    assert_eq!(run(bytes, Format::Docx), Err(OfficeError::Encrypted));
}

#[test]
fn ole_container_and_garbage_are_refused() {
    let mut cfb = vec![0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1];
    cfb.extend_from_slice(&[0u8; 512]);
    assert_eq!(run(cfb, Format::Docx), Err(OfficeError::NotPackage));
    assert_eq!(
        run(b"hello world".to_vec(), Format::Docx),
        Err(OfficeError::Corrupt)
    );
    assert_eq!(run(Vec::new(), Format::Xlsx), Err(OfficeError::Corrupt));
}

#[test]
fn large_text_is_cut_at_the_cap() {
    let limits = Limits {
        max_text_bytes: 1000,
        ..Limits::default()
    };
    let body: String = (0..500).map(|i| p(&format!("baris nomor {i} é"))).collect();
    let out = run_with(docx(&body), Format::Docx, limits).unwrap();
    assert!(out.truncated);
    let (kept, note) = out.text.split_once("[teks dipotong").unwrap();
    assert!(kept.len() <= 1001);
    assert!(note.contains("batas 0 KB") || note.contains("KB teks"));
    assert!(out.text.starts_with("baris nomor 0 é\n"));
}

#[test]
fn walk_stops_at_the_deadline() {
    let xml = "<a>".repeat(600) + &"</a>".repeat(600);
    let ctl = Ctl {
        deadline: Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap_or_else(Instant::now),
        max_depth: 1000,
        stop: &NEVER,
    };
    let result = walk(xml.as_bytes(), ctl, |_| Flow::Go);
    assert_eq!(result, Err(OfficeError::Timeout));
}

#[test]
fn relationship_targets_stay_inside_the_package() {
    assert_eq!(
        resolve_target("xl/", "worksheets/sheet1.xml"),
        "xl/worksheets/sheet1.xml"
    );
    assert_eq!(
        resolve_target("xl/", "/xl/worksheets/s.xml"),
        "xl/worksheets/s.xml"
    );
    assert_eq!(
        resolve_target("ppt/slides/", "../notesSlides/n1.xml"),
        "ppt/notesSlides/n1.xml"
    );
    assert_eq!(
        resolve_target("xl/", "../../../../Windows/win.ini"),
        "Windows/win.ini"
    );
}

// ---------------------------------------------------------------------------------------------
// XLSX
// ---------------------------------------------------------------------------------------------

const S: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

fn xlsx(sheets: &[(&str, &str)], shared: &[&str]) -> Vec<u8> {
    let sst: String = shared
        .iter()
        .map(|s| format!("<si><t>{s}</t></si>"))
        .collect();
    let sst = format!(r#"<sst {S} count="1">{sst}</sst>"#);
    let wb_sheets: String = sheets
        .iter()
        .enumerate()
        .map(|(i, (name, _))| {
            format!(
                r#"<sheet name="{name}" sheetId="{}" r:id="rId{}"/>"#,
                i + 1,
                i + 1
            )
        })
        .collect();
    let wb = format!(r#"<workbook {S}><sheets>{wb_sheets}</sheets></workbook>"#);
    // Sheets are deliberately stored in reverse file order to prove the rels are followed.
    let rels: String = (0..sheets.len())
        .map(|i| {
            format!(
                r#"<Relationship Id="rId{}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet{}.xml"/>"#,
                i + 1,
                sheets.len() - i
            )
        })
        .collect();
    let rels = format!(
        r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{rels}</Relationships>"#
    );
    let mut parts: Vec<(String, Vec<u8>)> = vec![
        ("xl/workbook.xml".into(), wb.into_bytes()),
        ("xl/_rels/workbook.xml.rels".into(), rels.into_bytes()),
        ("xl/sharedStrings.xml".into(), sst.into_bytes()),
    ];
    for (i, (_, data)) in sheets.iter().enumerate() {
        parts.push((
            format!("xl/worksheets/sheet{}.xml", sheets.len() - i),
            format!(r#"<worksheet {S}><sheetData>{data}</sheetData></worksheet>"#).into_bytes(),
        ));
    }
    let refs: Vec<(&str, &[u8])> = parts
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_slice()))
        .collect();
    package(&refs)
}

#[test]
fn xlsx_sheets_as_tsv_with_cached_values() {
    let data = r#"<row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row>
<row r="2"><c r="A2" t="s"><v>2</v></c><c r="B2"><f>SUM(1,29)</f><v>30</v></c><c r="D2" t="b"><v>1</v></c></row>
<row r="3"/>
<row r="4"><c r="B4" t="inlineStr"><is><t>inline	text</t></is></c><c r="C4" t="str"><f>"x"&amp;"y"</f><v>xy</v></c></row>"#;
    let other = r#"<row r="1"><c r="A1"><v>3.5</v></c></row>"#;
    let t = run(
        xlsx(
            &[("Data", data), ("Q&amp;A", other)],
            &["Name", "Age", "Alice"],
        ),
        Format::Xlsx,
    )
    .unwrap()
    .text;
    assert_eq!(
        t,
        "## Sheet: Data\nName\tAge\nAlice\t30\t\tTRUE\n\tinline text\txy\n\n## Sheet: Q&A\n3.5\n"
    );
    assert!(!t.contains("SUM"));
}

#[test]
fn xlsx_rows_and_columns_are_capped() {
    let limits = Limits {
        max_rows: 3,
        max_cols: 2,
        ..Limits::default()
    };
    let data: String = (1..=10)
        .map(|r| {
            format!(r#"<row r="{r}"><c r="A{r}"><v>{r}</v></c><c r="C{r}"><v>9</v></c></row>"#)
        })
        .collect();
    let t = run_with(xlsx(&[("S", &data)], &[]), Format::Xlsx, limits)
        .unwrap()
        .text;
    assert_eq!(
        t,
        "## Sheet: S\n1\n2\n3\n[dipotong: hanya 3 baris pertama]\n[kolom setelah kolom ke-2 tidak disertakan]\n"
    );
}

#[test]
fn column_letters() {
    assert_eq!(column_of("A1"), Some(0));
    assert_eq!(column_of("Z9"), Some(25));
    assert_eq!(column_of("AB12"), Some(27));
    assert_eq!(column_of("12"), None);
    assert!(column_of("ZZZZZZZZZZZZZZZZZZZZZZZZZZ1").is_some());
}

// ---------------------------------------------------------------------------------------------
// PPTX
// ---------------------------------------------------------------------------------------------

const P: &str = r#"xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main""#;

fn slide(paras: &[&str]) -> String {
    let body: String = paras
        .iter()
        .map(|t| format!("<a:p><a:r><a:t>{t}</a:t></a:r></a:p>"))
        .collect();
    format!(
        r#"<p:sld {P}><p:cSld><p:spTree><p:sp><p:txBody>{body}</p:txBody></p:sp></p:spTree></p:cSld></p:sld>"#
    )
}

#[test]
fn pptx_slides_in_number_order_with_notes() {
    let s1 = slide(&["Welcome", "Agenda"]);
    let s2 = format!(
        r#"<p:sld {P}><p:cSld><p:spTree><p:graphicFrame><a:graphic><a:graphicData><a:tbl><a:tr><a:tc><a:txBody><a:p><a:r><a:t>k</a:t></a:r></a:p></a:txBody></a:tc><a:tc><a:txBody><a:p><a:r><a:t>v</a:t></a:r></a:p></a:txBody></a:tc></a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame></p:spTree></p:cSld></p:sld>"#
    );
    let s10 = slide(&["Last"]);
    let rels = r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/notesSlide" Target="../notesSlides/notesSlide7.xml"/><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/slideLayout" Target="../slideLayouts/slideLayout1.xml"/></Relationships>"#;
    let notes = format!(
        r#"<p:notes {P}><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>Say hello</a:t></a:r></a:p></p:txBody></p:sp><p:sp><p:txBody><a:p><a:fld id="x" type="slidenum"><a:t>1</a:t></a:fld></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:notes>"#
    );
    let bytes = package(&[
        ("ppt/slides/slide10.xml", s10.as_bytes()),
        ("ppt/slides/slide2.xml", s2.as_bytes()),
        ("ppt/slides/slide1.xml", s1.as_bytes()),
        ("ppt/slides/_rels/slide1.xml.rels", rels.as_bytes()),
        ("ppt/notesSlides/notesSlide7.xml", notes.as_bytes()),
        (
            "ppt/slideLayouts/slideLayout1.xml",
            slide(&["LAYOUT"]).as_bytes(),
        ),
    ]);
    let t = run(bytes, Format::Pptx).unwrap().text;
    assert_eq!(
        t,
        "## Slide 1\nWelcome\nAgenda\nCatatan pembicara:\nSay hello\n\n## Slide 2\n| k | v |\n\n## Slide 10\nLast\n"
    );
}

// ---------------------------------------------------------------------------------------------
// OpenDocument
// ---------------------------------------------------------------------------------------------

const O: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0""#;

fn odf(body: &str) -> Vec<u8> {
    let content = format!(
        r#"<office:document-content {O}><office:body>{body}</office:body></office:document-content>"#
    );
    package(&[
        (
            "mimetype",
            b"application/vnd.oasis.opendocument.text".as_slice(),
        ),
        ("content.xml", content.as_bytes()),
    ])
}

#[test]
fn odt_headings_lists_tables() {
    let body = r#"<office:text><text:h text:outline-level="2">Title</text:h><text:p>a<text:s text:c="2"/>b<text:tab/>c</text:p><text:list><text:list-item><text:p>one</text:p></text:list-item></text:list><office:annotation><text:p>hidden note</text:p></office:annotation><table:table table:name="T"><table:table-row><table:table-cell><text:p>x</text:p></table:table-cell><table:table-cell table:number-columns-repeated="2"><text:p>y</text:p></table:table-cell></table:table-row></table:table></office:text>"#;
    let t = run(odf(body), Format::Odt).unwrap().text;
    assert_eq!(t, "## Title\na  b\tc\n- one\n| x | y | y |\n");
}

#[test]
fn ods_sheets_ignore_huge_empty_repeats() {
    let body = r#"<office:spreadsheet><table:table table:name="Sheet1"><table:table-row><table:table-cell><text:p>1</text:p></table:table-cell><table:table-cell table:number-columns-repeated="16380"/></table:table-row><table:table-row table:number-rows-repeated="1048570"><table:table-cell table:number-columns-repeated="16384"/></table:table-row></table:table></office:spreadsheet>"#;
    let t = run(odf(body), Format::Ods).unwrap().text;
    assert_eq!(t, "## Sheet: Sheet1\n1\n");
}

#[test]
fn odf_encrypted_manifest_is_refused() {
    let manifest = br#"<manifest:manifest><manifest:file-entry manifest:full-path="content.xml"><manifest:encryption-data/></manifest:file-entry></manifest:manifest>"#;
    let bytes = package(&[
        ("META-INF/manifest.xml", manifest.as_slice()),
        ("content.xml", b"garbage".as_slice()),
    ]);
    assert_eq!(run(bytes, Format::Odt), Err(OfficeError::Encrypted));
}

// ---------------------------------------------------------------------------------------------
// Media
// ---------------------------------------------------------------------------------------------

fn png(n: u8) -> Vec<u8> {
    let mut b = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    b.extend_from_slice(&[n; 32]);
    b
}

#[test]
fn media_over_the_limits_is_left_out() {
    let mut parts: Vec<(String, Vec<u8>)> = (1..=25)
        .map(|i| (format!("word/media/image{i}.png"), png(i as u8)))
        .collect();
    parts.push(("word/media/fake.png".into(), b"MZ not an image".to_vec()));
    parts.push((
        "word/media/photo.jpeg".into(),
        vec![0xFF, 0xD8, 0xFF, 0xE0, 1, 2],
    ));
    parts.push(("word/media/vector.emf".into(), vec![1, 2, 3]));
    parts.push(("word/document.xml".into(), document(&p("x")).into_bytes()));
    let refs: Vec<(&str, &[u8])> = parts
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_slice()))
        .collect();
    let bytes = package(&refs);
    let set = media_from(Cursor::new(bytes.clone()), Format::Docx, &Limits::default()).unwrap();
    assert_eq!(set.items.len(), 20);
    assert_eq!(set.items[0].name, "image001.png");
    assert_eq!(set.items[0].bytes, png(1));
    assert_eq!(set.items[19].name, "image020.png");
    assert_eq!(set.items[19].bytes, png(20));
    assert_eq!(set.skipped, 25 + 2 - 20);
    assert!(set.items.iter().all(|m| image_ext(&m.bytes).is_some()));

    let small = Limits {
        max_media_bytes: 100,
        ..Limits::default()
    };
    let set = media_from(Cursor::new(bytes), Format::Docx, &small).unwrap();
    // image1 + image2 (40 bytes each) fit, the next PNGs do not; the small JPEG still does.
    let names: Vec<&str> = set.items.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["image001.png", "image002.png", "image003.jpg"]);
    assert_eq!(set.items.iter().map(|m| m.bytes.len()).sum::<usize>(), 86);
}

#[test]
fn o3_media_names_are_synthetic_never_from_the_zip() {
    let gif = b"GIF89a\x01\x00\x01\x00".to_vec();
    let bytes = package(&[
        ("ppt/media/CON.png", png(1).as_slice()),
        ("ppt/media/nul.PNG", png(2).as_slice()),
        ("ppt/media/COM1.png", png(3).as_slice()),
        ("ppt/media/lpt9.gif", gif.as_slice()),
        ("ppt/media/AUX.jpg", png(5).as_slice()),
        ("ppt/media/im age$.png", png(6).as_slice()),
        ("ppt/media/sub/deep.png", png(7).as_slice()),
    ]);
    let set = media_from(Cursor::new(bytes), Format::Pptx, &Limits::default()).unwrap();
    let names: Vec<&str> = set.items.iter().map(|m| m.name.as_str()).collect();
    // Sorted by member name; the extension comes from the picture's own signature.
    assert_eq!(
        names,
        [
            "image001.png",
            "image002.png",
            "image003.png",
            "image004.png",
            "image005.gif",
            "image006.png"
        ]
    );
    for n in &names {
        let stem = n.split('.').next().unwrap().to_ascii_uppercase();
        assert!(
            !["CON", "PRN", "AUX", "NUL"].contains(&stem.as_str())
                && !stem.starts_with("COM")
                && !stem.starts_with("LPT")
        );
    }
    let none = media_from(Cursor::new(Vec::new()), Format::Xlsx, &Limits::default()).unwrap();
    assert!(none.items.is_empty());
}

// ---------------------------------------------------------------------------------------------
// Review O1/O2/O4
// ---------------------------------------------------------------------------------------------

fn stored_package(parts: &[(&str, &[u8])]) -> Vec<u8> {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .large_file(false);
    for (name, data) in parts {
        w.start_file(*name, opts).unwrap();
        w.write_all(data).unwrap();
    }
    w.finish().unwrap().into_inner()
}

/// Pseudo-random letters (incompressible enough to pass the ratio check).
fn noise(len: usize) -> String {
    let mut x: u32 = 0x1234_5678;
    (0..len)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (b'a' + (x % 26) as u8) as char
        })
        .collect()
}

#[test]
fn o1_one_huge_shared_string_referenced_by_10k_cells_stays_bounded() {
    let big = noise(30 * 1024 * 1024 - 1024);
    let sst = format!(r#"<sst {S}><si><t>{big}</t></si></sst>"#);
    drop(big);
    let mut sheet = String::from("<sheetData>");
    for r in 1..=100 {
        sheet.push_str(&format!(r#"<row r="{r}">"#));
        for c in 0..100 {
            sheet.push_str(&format!(r#"<c r="{}{r}" t="s"><v>0</v></c>"#, col_name(c)));
        }
        sheet.push_str("</row>");
    }
    sheet.push_str("</sheetData>");
    let wb = format!(r#"<workbook {S}><sheets><sheet name="S" sheetId="1"/></sheets></workbook>"#);
    let ws = format!(r#"<worksheet {S}>{sheet}</worksheet>"#);
    let bytes = stored_package(&[
        ("xl/sharedStrings.xml", sst.as_bytes()),
        ("xl/workbook.xml", wb.as_bytes()),
        ("xl/worksheets/sheet1.xml", ws.as_bytes()),
    ]);
    drop(sst);
    let started = Instant::now();
    let out = run(bytes, Format::Xlsx).unwrap();
    let took = started.elapsed();
    assert!(took < TIMEOUT, "took {took:?}");
    assert!(out.truncated);
    assert!(out.text.len() <= MAX_TEXT_BYTES + 100);
    // Every kept cell is cut at the item cap (32 KB + mark), never the whole 30 MB.
    let first_row = out.text.lines().nth(1).unwrap();
    let first_cell = first_row.split('\t').next().unwrap();
    assert_eq!(first_cell.len(), MAX_ITEM_BYTES + " […]".len());
}

fn col_name(mut c: usize) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + (c % 26) as u8);
        if c < 26 {
            break;
        }
        c = c / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).unwrap()
}

#[test]
fn o1_cells_past_the_column_limit_are_never_copied() {
    // 60 columns of a 20 KB shared string: only 50 are kept, the rest are marked.
    let text = noise(20 * 1024);
    let row: String = (0..60)
        .map(|c| format!(r#"<c r="{}1" t="s"><v>0</v></c>"#, col_name(c)))
        .collect();
    let out = run(
        xlsx(&[("S", &format!(r#"<row r="1">{row}</row>"#))], &[&text]),
        Format::Xlsx,
    )
    .unwrap()
    .text;
    let line = out.lines().nth(1).unwrap();
    assert_eq!(line.split('\t').count(), 50);
    assert!(out.contains("[kolom setelah kolom ke-50 tidak disertakan]"));
}

#[test]
fn o1_ods_extreme_repeats_are_bounded() {
    let cell = noise(8 * 1024);
    let body = format!(
        r#"<office:spreadsheet><table:table table:name="S"><table:table-row table:number-rows-repeated="1000000000"><table:table-cell table:number-columns-repeated="1000000000"><text:p>{cell}</text:p></table:table-cell></table:table-row></table:table></office:spreadsheet>"#
    );
    let started = Instant::now();
    let out = run(odf(&body), Format::Ods).unwrap();
    assert!(started.elapsed() < TIMEOUT);
    assert!(out.truncated);
    assert!(out.text.len() <= MAX_TEXT_BYTES + 100);
    let line = out.text.lines().nth(1).unwrap();
    assert!(line.split('\t').count() <= 50);
}

#[test]
fn o1_nested_docx_tables_do_not_multiply() {
    // 40 levels of nested tables, each cell holding a 20 KB paragraph: every level is bounded.
    let para = p(&noise(20 * 1024));
    let mut body = String::new();
    for _ in 0..40 {
        body.push_str("<w:tbl><w:tr><w:tc>");
        body.push_str(&para);
    }
    for _ in 0..40 {
        body.push_str("</w:tc></w:tr></w:tbl>");
    }
    let out = run(docx(&body), Format::Docx).unwrap();
    assert!(out.text.len() <= MAX_ITEM_BYTES + 64, "{}", out.text.len());
}

#[test]
fn o1_a_giant_paragraph_is_cut_with_a_mark() {
    let out = run(docx(&p(&noise(100 * 1024))), Format::Docx).unwrap();
    assert!(out.text.trim_end().ends_with("[…]"));
    assert!(out.text.len() <= MAX_ITEM_BYTES + 16);
}

/// Rewrites the classic EOCD fields of a zip built in the test.
fn patch_eocd(bytes: &mut [u8], entries: Option<u16>, cd_size: Option<u32>) {
    let at = (0..bytes.len() - 21)
        .rev()
        .find(|&i| &bytes[i..i + 4] == b"PK\x05\x06")
        .unwrap();
    if let Some(n) = entries {
        bytes[at + 8..at + 10].copy_from_slice(&n.to_le_bytes());
        bytes[at + 10..at + 12].copy_from_slice(&n.to_le_bytes());
    }
    if let Some(s) = cd_size {
        bytes[at + 12..at + 16].copy_from_slice(&s.to_le_bytes());
    }
}

#[test]
fn o2_entry_count_and_central_directory_size_are_read_before_parsing() {
    let doc = document(&p("x"));
    let base = package(&[("word/document.xml", doc.as_bytes())]);
    let mut many = base.clone();
    patch_eocd(&mut many, Some(20_000), None);
    assert_eq!(
        run(many, Format::Docx),
        Err(OfficeError::Limit("jumlah entri zip"))
    );
    let mut big_cd = base.clone();
    patch_eocd(&mut big_cd, None, Some(9 * 1024 * 1024));
    assert_eq!(
        run(big_cd, Format::Docx),
        Err(OfficeError::Limit("ukuran central directory"))
    );
    // Saturated fields without a ZIP64 locator are refused, not trusted.
    let mut fake64 = base.clone();
    patch_eocd(&mut fake64, Some(0xFFFF), None);
    assert!(run(fake64, Format::Docx).is_err());
    let small = Limits {
        max_cd_bytes: 16,
        ..Limits::default()
    };
    assert_eq!(
        run_with(base.clone(), Format::Docx, small),
        Err(OfficeError::Limit("ukuran central directory"))
    );
    assert!(run(base, Format::Docx).is_ok());
}

#[test]
fn o2b_a_small_fake_eocd_in_the_comment_does_not_hide_the_real_one() {
    // Fake EOCD (1 entry, tiny CD, no comment) placed in the zip comment, i.e. AFTER the real one.
    let mut fake = Vec::new();
    fake.extend_from_slice(b"PK\x05\x06");
    fake.extend_from_slice(&[0, 0, 0, 0]); // disk numbers
    fake.extend_from_slice(&1u16.to_le_bytes());
    fake.extend_from_slice(&1u16.to_le_bytes());
    fake.extend_from_slice(&46u32.to_le_bytes());
    fake.extend_from_slice(&0u32.to_le_bytes());
    fake.extend_from_slice(&0u16.to_le_bytes());
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    w.set_raw_comment(fake.into_boxed_slice());
    w.start_file(
        "word/document.xml",
        zip::write::SimpleFileOptions::default(),
    )
    .unwrap();
    w.write_all(document(&p("x")).as_bytes()).unwrap();
    let base = w.finish().unwrap().into_inner();
    let sigs: Vec<usize> = (0..base.len() - 3)
        .filter(|&i| &base[i..i + 4] == b"PK\x05\x06")
        .collect();
    assert_eq!(sigs.len(), 2, "real EOCD + fake one in the comment");
    // Untouched, the pre-check lets it through (small counts everywhere); whatever the zip crate
    // then makes of the fake record is its own business.
    assert!(!matches!(
        run(base.clone(), Format::Docx),
        Err(OfficeError::Limit(_))
    ));
    // The REAL (first) EOCD claims 20 000 entries: refused although the last one says 1.
    let mut many = base.clone();
    let real = sigs[0];
    many[real + 8..real + 10].copy_from_slice(&20_000u16.to_le_bytes());
    many[real + 10..real + 12].copy_from_slice(&20_000u16.to_le_bytes());
    assert_eq!(
        run(many, Format::Docx),
        Err(OfficeError::Limit("jumlah entri zip"))
    );
    // Same for a huge central directory size.
    let mut big = base;
    big[real + 12..real + 16].copy_from_slice(&(64u32 << 20).to_le_bytes());
    assert_eq!(
        run(big, Format::Docx),
        Err(OfficeError::Limit("ukuran central directory"))
    );
}

#[test]
fn o2_zip64_eocd_is_read() {
    let mut w = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default().large_file(true);
    w.start_file("word/document.xml", opts).unwrap();
    w.write_all(document(&p("zip64")).as_bytes()).unwrap();
    let bytes = w.finish().unwrap().into_inner();
    let out = run(bytes.clone(), Format::Docx);
    if bytes.windows(4).any(|x| x == b"PK\x06\x06") {
        assert_eq!(out.unwrap().text, "zip64\n");
    } else {
        assert!(out.is_ok());
    }
}

#[test]
fn o4_a_raised_cancel_flag_stops_extraction() {
    let stop = AtomicBool::new(true);
    let r = extract_with(
        Cursor::new(docx(&p("x"))),
        Format::Docx,
        &Limits::default(),
        &stop,
    );
    assert_eq!(r, Err(OfficeError::Cancelled));
}

#[test]
fn o4_text_and_media_in_one_pass() {
    let doc = document(&p("hello"));
    let pic = png(9);
    let bytes = package(&[
        ("word/document.xml", doc.as_bytes()),
        ("word/media/a.png", pic.as_slice()),
    ]);
    let c = content_with(Cursor::new(bytes), Format::Docx, &Limits::default(), &NEVER).unwrap();
    assert_eq!(c.text.unwrap().text, "hello\n");
    assert_eq!(c.media.items.len(), 1);
    assert_eq!(c.media.items[0].name, "image001.png");
}

/// Manual check on a real document (never runs in CI): prints only the character count, the
/// first heading and the picture count, never the text.
/// `FERRUM_OFFICE_SAMPLE=C:\path\file.docx cargo test --lib office_text::tests::sample -- --ignored --nocapture`
#[test]
#[ignore]
fn sample() {
    let Ok(path) = std::env::var("FERRUM_OFFICE_SAMPLE") else {
        return;
    };
    let file = crate::path_guard::checked_file(&path).unwrap();
    let format = format_by_name(&path).unwrap();
    let content = content_file(&file, format, &NEVER).unwrap();
    let out = content.text.unwrap();
    let heading = out.text.lines().find(|l| l.starts_with('#')).unwrap_or("");
    let media = content.media;
    println!(
        "chars={} bytes={} truncated={} first_heading={heading:?} media={} skipped={}",
        out.text.chars().count(),
        out.text.len(),
        out.truncated,
        media.items.len(),
        media.skipped
    );
}
