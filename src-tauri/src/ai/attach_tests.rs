// FER-486: bounded attachment extraction and the sensitive-item guard.
use super::*;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ferrum-fer486-{tag}-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, name: &str, bytes: &[u8]) -> String {
    let p = dir.join(name);
    fs::write(&p, bytes).unwrap();
    p.to_string_lossy().into_owned()
}

fn r(path: &str) -> AttachmentRef {
    AttachmentRef {
        path: path.to_owned(),
        confirmed_sensitive: false,
    }
}

#[test]
fn text_is_read_as_lossy_utf8_without_bom() {
    let dir = temp_dir("text");
    let p = write(&dir, "notes.txt", "\u{feff}héllo wörld".as_bytes());
    let out = extract(&[r(&p)]);
    assert_eq!(out[0].info.kind, AttachmentKind::Text);
    assert_eq!(out[0].info.support, Support::Content);
    assert_eq!(out[0].body.as_deref(), Some("héllo wörld"));
    assert_eq!(out[0].truncated, None);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_large_text_file_is_cut_at_the_per_file_limit_with_its_sizes() {
    let dir = temp_dir("big");
    let p = write(&dir, "big.log", &vec![b'a'; MAX_FILE_BYTES + 5000]);
    let out = extract(&[r(&p)]);
    assert_eq!(out[0].body.as_ref().unwrap().len(), MAX_FILE_BYTES);
    assert_eq!(
        out[0].truncated,
        Some((MAX_FILE_BYTES as u64, (MAX_FILE_BYTES + 5000) as u64))
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_cut_never_splits_a_multibyte_character() {
    let mut bytes = vec![b'a'; MAX_FILE_BYTES - 1];
    bytes.extend("é".as_bytes()); // 2 bytes straddling the limit
    bytes.extend(b"tail");
    let cut = cut_utf8(&bytes, MAX_FILE_BYTES);
    assert_eq!(cut.len(), MAX_FILE_BYTES - 1);
    assert!(std::str::from_utf8(cut).is_ok());
}

#[test]
fn the_total_budget_spans_all_files_and_later_files_get_metadata_only() {
    let dir = temp_dir("budget");
    let refs: Vec<AttachmentRef> = (0..4)
        .map(|i| {
            r(&write(
                &dir,
                &format!("f{i}.txt"),
                &vec![b'x'; MAX_FILE_BYTES + 10],
            ))
        })
        .collect();
    let out = extract(&refs);
    let total: usize = out
        .iter()
        .filter_map(|e| e.body.as_ref())
        .map(String::len)
        .sum();
    assert_eq!(total, MAX_TOTAL_BYTES);
    assert!(out[..3].iter().all(|e| e.info.support == Support::Content));
    assert_eq!(out[3].info.support, Support::Metadata);
    assert_eq!(out[3].info.note.as_deref(), Some(NOTE_BUDGET_SPENT));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn binary_pdf_legacy_office_and_images_are_metadata_only() {
    let dir = temp_dir("meta");
    let files = [
        (
            "blob.unknownext",
            b"MZ\0\0\x01binary".to_vec(),
            AttachmentKind::Binary,
        ),
        ("doc.pdf", b"%PDF-1.7 ...".to_vec(), AttachmentKind::Pdf),
        (
            "legacy.doc",
            b"\xD0\xCF\x11\xE0".to_vec(),
            AttachmentKind::Document,
        ),
        ("photo.png", b"\x89PNG".to_vec(), AttachmentKind::Image),
        ("pack.zip", b"PK\x03\x04".to_vec(), AttachmentKind::Archive),
    ];
    for (name, bytes, kind) in files {
        let p = write(&dir, name, &bytes);
        let out = extract(&[r(&p)]);
        assert_eq!(out[0].info.kind, kind, "{name}");
        assert_eq!(out[0].info.support, Support::Metadata, "{name}");
        assert_eq!(out[0].body, None, "{name}");
        assert_eq!(
            out[0].info.note.as_deref(),
            Some(NOTE_METADATA_ONLY),
            "{name}"
        );
    }
    let _ = fs::remove_dir_all(dir);
}

// FER-493: Office documents become extracted text.

fn office_file(dir: &Path, name: &str, parts: &[(&str, &[u8])]) -> String {
    use std::io::Write;
    let p = dir.join(name);
    let mut zip = zip::ZipWriter::new(fs::File::create(&p).unwrap());
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    for (part, data) in parts {
        zip.start_file(*part, opts).unwrap();
        zip.write_all(data).unwrap();
    }
    zip.finish().unwrap();
    p.to_string_lossy().into_owned()
}

fn word(paragraphs: &[&str]) -> Vec<u8> {
    let body: String = paragraphs
        .iter()
        .map(|t| format!("<w:p><w:r><w:t>{t}</w:t></w:r></w:p>"))
        .collect();
    format!(
        r#"<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Getting Started</w:t></w:r></w:p>{body}</w:body></w:document>"#
    )
    .into_bytes()
}

#[test]
fn office_text_is_extracted_and_sent_like_a_text_file() {
    let dir = temp_dir("office");
    let p = office_file(
        &dir,
        "guide.docx",
        &[("word/document.xml", &word(&["Hello"]))],
    );
    let out = extract(&[r(&p)]);
    let item = &out[0];
    assert_eq!(item.info.kind, AttachmentKind::Document);
    assert_eq!(item.info.support, Support::Content);
    assert_eq!(item.info.note.as_deref(), Some(NOTE_OFFICE_TEXT));
    assert_eq!(item.info.sensitive, None);
    let body = item.body.as_deref().unwrap();
    assert!(body.starts_with(OFFICE_TEXT_HEADER), "{body}");
    assert!(body.ends_with("# Getting Started\nHello\n"), "{body}");
    assert_eq!(item.truncated, None);

    // The chip keeps the "teks diekstrak" note; content never reaches the webview.
    let chips = inspect(std::slice::from_ref(&p)).unwrap();
    let info = chips[0].info.as_ref().unwrap();
    assert_eq!(info.support, Support::Content);
    assert_eq!(info.note.as_deref(), Some(NOTE_OFFICE_TEXT));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn office_secret_in_compressed_text_is_flagged() {
    let dir = temp_dir("office-secret");
    let token = format!("ghp_{}", "A1b2C3d4E5".repeat(4));
    let p = office_file(
        &dir,
        "notes.docx",
        &[("word/document.xml", &word(&[&format!("token = {token}")]))],
    );
    // The zip is compressed: the raw bytes never show the token, the extracted text does.
    assert!(!sensitive_content(&String::from_utf8_lossy(
        &fs::read(&p).unwrap()
    )));
    let chips = inspect(std::slice::from_ref(&p)).unwrap();
    assert_eq!(
        chips[0].info.as_ref().unwrap().sensitive,
        Some(SensitiveReason::Content)
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn office_text_is_cut_to_the_per_file_and_total_budget() {
    let dir = temp_dir("office-big");
    let line = "x".repeat(1000);
    let lines: Vec<&str> = std::iter::repeat_n(line.as_str(), 400).collect();
    let doc = word(&lines);
    let refs: Vec<AttachmentRef> = (0..4)
        .map(|i| {
            r(&office_file(
                &dir,
                &format!("big{i}.docx"),
                &[("word/document.xml", &doc)],
            ))
        })
        .collect();
    let out = extract(&refs);
    let first = &out[0];
    assert_eq!(first.body.as_ref().unwrap().len(), MAX_FILE_BYTES);
    let (sent, total) = first.truncated.unwrap();
    assert_eq!(sent as usize, MAX_FILE_BYTES - OFFICE_TEXT_HEADER.len());
    assert!(total > 400_000);
    let sum: usize = out
        .iter()
        .filter_map(|e| e.body.as_ref())
        .map(String::len)
        .sum();
    assert_eq!(sum, MAX_TOTAL_BYTES);
    assert_eq!(out[3].info.support, Support::Metadata);
    assert_eq!(out[3].info.note.as_deref(), Some(NOTE_BUDGET_SPENT));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn office_without_text_or_broken_stays_metadata_with_a_reason() {
    let dir = temp_dir("office-empty");
    let empty = office_file(
        &dir,
        "blank.docx",
        &[(
            "word/document.xml",
            br#"<w:document xmlns:w="w"><w:body><w:p/></w:body></w:document>"#.as_slice(),
        )],
    );
    let broken = write(&dir, "broken.xlsx", b"PK\x03\x04 not really");
    let bomb_doc = format!(
        r#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>{}</w:t></w:r></w:p></w:body></w:document>"#,
        " ".repeat(3 * 1024 * 1024)
    );
    let bomb = office_file(
        &dir,
        "bomb.docx",
        &[("word/document.xml", bomb_doc.as_bytes())],
    );
    let out = extract(&[r(&empty), r(&broken), r(&bomb)]);
    assert_eq!(out[0].info.support, Support::Metadata);
    assert_eq!(out[0].info.note.as_deref(), Some(NOTE_OFFICE_EMPTY));
    for item in &out[1..] {
        assert_eq!(item.info.kind, AttachmentKind::Document);
        assert_eq!(item.info.support, Support::Metadata);
        assert_eq!(item.body, None);
        assert!(
            item.info
                .note
                .as_deref()
                .unwrap()
                .starts_with("teks tidak dapat diekstrak: "),
            "{:?}",
            item.info.note
        );
    }
    assert!(
        out[2]
            .info
            .note
            .as_deref()
            .unwrap()
            .contains("rasio kompresi")
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_folder_sends_names_and_sizes_only_and_caps_the_listing() {
    let dir = temp_dir("folder");
    write(&dir, "secret-content.txt", b"TOP SECRET BODY");
    fs::create_dir(dir.join("sub")).unwrap();
    let out = extract(&[r(dir.to_str().unwrap())]);
    let body = out[0].body.as_deref().unwrap();
    assert_eq!(out[0].info.kind, AttachmentKind::Folder);
    assert_eq!(out[0].info.support, Support::Listing);
    assert!(body.contains("- sub/ (folder)"));
    assert!(body.contains("- secret-content.txt (15 byte)"));
    assert!(!body.contains("TOP SECRET BODY"));

    let many = temp_dir("many");
    for i in 0..(MAX_LISTING_ENTRIES + 7) {
        write(&many, &format!("e{i:04}.txt"), b"");
    }
    let out = extract(&[r(many.to_str().unwrap())]);
    let body = out[0].body.as_deref().unwrap();
    assert_eq!(
        body.lines().filter(|l| l.starts_with("- ")).count(),
        MAX_LISTING_ENTRIES
    );
    assert!(body.contains(&format!(
        "[daftar dipotong: {} dari {} entri]",
        MAX_LISTING_ENTRIES,
        MAX_LISTING_ENTRIES + 7
    )));
    let _ = fs::remove_dir_all(dir);
    let _ = fs::remove_dir_all(many);
}

#[test]
fn sensitive_names_are_flagged() {
    for name in [
        ".env",
        ".env.local",
        "prod.env",
        "server.pem",
        "tls.key",
        "id_rsa",
        "id_ed25519.pub",
        "cert.pfx",
        "credentials",
        "credentials.json",
        "aws-credentials.txt",
        "vault.kdbx",
        "wallet.dat",
        "wallet.json",
        ".npmrc",
        ".git-credentials",
        "secrets.yaml",
        "main.tfvars",
    ] {
        assert!(sensitive_name(name), "{name}");
    }
    for name in [
        "notes.txt",
        "README.md",
        "environment.md",
        "keyboard.txt",
        "main.rs",
    ] {
        assert!(!sensitive_name(name), "{name}");
    }
}

/// FER-486 A2: coverage added after the security review, one sample per pattern.
#[test]
fn review_a2_token_patterns_are_flagged() {
    for text in [
        "-----BEGIN PGP PRIVATE KEY BLOCK-----",
        "stripe = sk_live_51Habcdefghijklmnop0123",
        "restricted rk_live_51Habcdefghijklmnop0123",
        "DefaultEndpointsProtocol=https;AccountName=x;AccountKey=abcdEFGH1234abcdEFGH1234abcd==;",
        "Endpoint=sb://x.servicebus.windows.net/;SharedAccessKeyName=k;SharedAccessKey=abc123def456ghi=",
        "SharedAccessSignature=sv=2022-11-02&sig=abcdef0123456789",
        "//registry.npmjs.org/:_authToken=npm_abcdefghijklmnopqrstuvwxyz0123456789",
        "HF_TOKEN=hf_abcdefghijklmnopqrstuvwxyzABCDEFGH",
        "Authorization: ya29.a0AfH6SMBabcdefghijklmnop",
        "SLACK_APP_TOKEN=xapp-1-A0123456789-0123456789-abcdef",
    ] {
        assert!(sensitive_content(text), "{text}");
    }
    for text in [
        "-----BEGIN PGP PUBLIC KEY BLOCK-----",
        "skip_live_preview",
        "the AccountKey field",
        "npm_install.log",
        "hf_short",
    ] {
        assert!(!sensitive_content(text), "{text}");
    }
}

#[test]
fn token_patterns_in_content_are_flagged() {
    for text in [
        "token = dapi0123456789abcdef0123456789abcdef",
        "OPENAI=sk-proj-abcdefghijklmnopqrstuvwx",
        "key AKIAABCDEFGHIJKLMNOP here",
        "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
        "github_pat_11ABCDEFG0123456789_abcdefghij",
        "-----BEGIN OPENSSH PRIVATE KEY-----",
        "-----BEGIN RSA PRIVATE KEY-----",
        "xoxb-1234567890-abcdefghij",
        "AIzaSyA1234567890abcdefghijklmnopqrstuv",
        "password: hunter2hunter2",
        "\"api_key\": \"0123456789abcdef\"",
        "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.signature",
    ] {
        assert!(sensitive_content(text), "{text}");
    }
    for text in [
        "Rapat hari Senin membahas anggaran.",
        "the password field is required",
        "sk-short",
        "skip this",
    ] {
        assert!(!sensitive_content(text), "{text}");
    }
}

#[test]
fn inspect_flags_name_and_content_and_never_returns_content() {
    let dir = temp_dir("inspect");
    let env = write(&dir, ".env", b"PLAIN=1");
    let leak = write(
        &dir,
        "config.txt",
        b"x\nAPI=dapi0123456789abcdef0123456789abcdef\n",
    );
    let ok = write(&dir, "notes.txt", b"hello");
    let out = inspect(&[env.clone(), leak.clone(), ok.clone()]).unwrap();
    assert_eq!(
        out[0].info.as_ref().unwrap().sensitive,
        Some(SensitiveReason::Name)
    );
    assert_eq!(
        out[1].info.as_ref().unwrap().sensitive,
        Some(SensitiveReason::Content)
    );
    assert_eq!(out[2].info.as_ref().unwrap().sensitive, None);
    let json = serde_json::to_string(&out).unwrap();
    assert!(!json.contains("dapi0123456789"));
    assert!(!json.contains("hello"));
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn inspect_refuses_more_than_ten_paths_and_reports_missing_files() {
    let paths: Vec<String> = (0..11).map(|i| format!("C:\\nope\\{i}.txt")).collect();
    assert!(inspect(&paths).is_err());
    let out = inspect(&paths[..1]).unwrap();
    assert!(out[0].info.is_none());
    assert!(out[0].error.is_some());
}

#[test]
fn relative_and_device_paths_are_refused() {
    for path in [
        "notes.txt",
        r"..\x.txt",
        r"\\.\PhysicalDrive0",
        r"\\?\C:\x.txt",
        "",
    ] {
        assert!(locate(path).is_err(), "{path}");
    }
}

#[test]
fn a_path_inside_a_zip_reads_the_member_through_the_bounded_preview() {
    use std::io::Write;
    let dir = temp_dir("zip");
    let zip_path = dir.join("pack.zip");
    {
        let file = fs::File::create(&zip_path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
        zip.start_file("docs/readme.txt", opts).unwrap();
        zip.write_all(b"inside the archive").unwrap();
        zip.finish().unwrap();
    }
    let member = format!("{}\\docs\\readme.txt", zip_path.to_string_lossy());
    let out = extract(&[r(&member)]);
    assert!(out[0].info.in_archive, "{:?}", out[0].info);
    assert_eq!(out[0].info.kind, AttachmentKind::Text);
    assert_eq!(out[0].body.as_deref(), Some("inside the archive"));

    let folder = format!("{}\\docs", zip_path.to_string_lossy());
    let out = extract(&[r(&folder)]);
    assert_eq!(out[0].info.kind, AttachmentKind::Folder);
    assert!(
        out[0]
            .body
            .as_deref()
            .unwrap()
            .contains("- readme.txt (18 byte)")
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn an_unreadable_file_at_send_time_becomes_a_note() {
    let out = extract(&[r(r"C:\definitely\missing\file.txt")]);
    assert_eq!(out[0].info.support, Support::Metadata);
    assert!(
        out[0]
            .info
            .note
            .as_deref()
            .unwrap()
            .starts_with("tidak dapat dibaca")
    );
}

// FER-493 review O4: short inspect deadline, cancel, cache.

#[test]
fn o4_inspect_timeout_defers_and_cancel_stops() {
    let dir = temp_dir("office-o4");
    let p = office_file(&dir, "slow.docx", &[("word/document.xml", &word(&["Hi"]))]);
    let entry = crate::path_guard::open_entry(&p).unwrap();
    let late = read_disk_item(
        &p,
        entry,
        MAX_FILE_BYTES,
        OfficeRun {
            timeout: Duration::ZERO,
            stop: &NO_STOP,
            inspecting: true,
        },
    )
    .unwrap();
    assert_eq!(late.info.support, Support::Metadata);
    assert_eq!(late.info.note.as_deref(), Some(NOTE_OFFICE_LATER));

    let stop = AtomicBool::new(true);
    let chips = inspect_cancellable(std::slice::from_ref(&p), &[&stop]).unwrap();
    let note = chips[0].info.as_ref().unwrap().note.clone().unwrap();
    assert!(note.contains("dibatalkan"), "{note}");

    // Neither the timeout nor the cancel was cached: a normal send extracts the text.
    let out = extract(&[r(&p)]);
    assert_eq!(out[0].info.support, Support::Content);
    // Changing the file (size/mtime) invalidates the cached text.
    std::thread::sleep(Duration::from_millis(20));
    office_file(
        &dir,
        "slow.docx",
        &[("word/document.xml", &word(&["Changed text"]))],
    );
    let out = extract(&[r(&p)]);
    assert!(out[0].body.as_deref().unwrap().contains("Changed text"));
    let _ = fs::remove_dir_all(dir);
}
