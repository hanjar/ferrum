//! FER-490 E1 (snapshot limits and refusals) and E3 (executable location ACL).

use std::path::Path;

use super::acl::trusted_location;
use super::snapshot::{FILES_DIR, Limits, MAX_BYTES, MAX_FILES, take};
use crate::ai::attach::AttachmentRef;

fn r(path: &Path, confirmed: bool) -> (AttachmentRef, String) {
    let p = path.to_string_lossy().into_owned();
    (
        AttachmentRef {
            path: p.clone(),
            confirmed_sensitive: confirmed,
        },
        p,
    )
}

#[test]
fn a_file_copy_contains_only_that_file() {
    let src = tempfile::tempdir().unwrap();
    std::fs::write(src.path().join("a.txt"), "AAA").unwrap();
    std::fs::write(src.path().join("b.txt"), "BBB").unwrap();
    let work = tempfile::tempdir().unwrap();
    let snap = take(
        &[r(&src.path().join("a.txt"), false)],
        work.path(),
        Limits::default(),
    )
    .unwrap();
    let files = work.path().join(FILES_DIR);
    let names: Vec<_> = std::fs::read_dir(&files)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, vec!["a.txt"]);
    assert_eq!(std::fs::read_to_string(files.join("a.txt")).unwrap(), "AAA");
    assert_eq!(snap.items[0].path, r"files\a.txt");
    assert_eq!((snap.files, snap.bytes), (1, 3));
}

#[test]
fn same_names_from_two_folders_do_not_collide() {
    let src = tempfile::tempdir().unwrap();
    for d in ["x", "y"] {
        std::fs::create_dir_all(src.path().join(d)).unwrap();
        std::fs::write(src.path().join(d).join("a.txt"), d).unwrap();
    }
    let work = tempfile::tempdir().unwrap();
    let snap = take(
        &[
            r(&src.path().join("x").join("a.txt"), false),
            r(&src.path().join("y").join("a.txt"), false),
        ],
        work.path(),
        Limits::default(),
    )
    .unwrap();
    let paths: Vec<_> = snap.items.iter().map(|i| i.path.clone()).collect();
    assert_eq!(paths, vec![r"files\a.txt", r"files\a.txt (2)"]);
}

#[test]
fn an_unticked_secret_is_never_copied_and_a_ticked_one_is() {
    let src = tempfile::tempdir().unwrap();
    let env = src.path().join(".env");
    std::fs::write(&env, "X=1").unwrap();
    // A secret far past the first 100 KB is found too (the whole file is scanned).
    let late = src.path().join("log.txt");
    let mut text = "a".repeat(300 * 1024);
    text.push_str("\nAKIAABCDEFGHIJKLMNOP\n");
    std::fs::write(&late, text).unwrap();
    for f in [&env, &late] {
        let work = tempfile::tempdir().unwrap();
        let e = take(&[r(f, false)], work.path(), Limits::default()).unwrap_err();
        assert_eq!(e.kind, "sensitive_unconfirmed", "{}", f.display());
        assert!(
            std::fs::read_dir(work.path().join(FILES_DIR))
                .unwrap()
                .next()
                .is_none()
        );
    }
    let work = tempfile::tempdir().unwrap();
    take(&[r(&env, true)], work.path(), Limits::default()).unwrap();
    assert!(work.path().join(FILES_DIR).join(".env").is_file());
}

#[test]
fn limits_100_mb_2000_files_and_depth_8() {
    assert_eq!(MAX_BYTES, 100 * 1024 * 1024);
    assert_eq!(MAX_FILES, 2000);
    let src = tempfile::tempdir().unwrap();
    // 100 MB + 1 byte: refused before reading (size check), nothing copied.
    let big = src.path().join("big.bin");
    std::fs::File::create(&big)
        .unwrap()
        .set_len(MAX_BYTES + 1)
        .unwrap();
    let work = tempfile::tempdir().unwrap();
    let e = take(&[r(&big, false)], work.path(), Limits::default()).unwrap_err();
    assert_eq!(e.kind, "snapshot_limit");
    assert_eq!(e.message.as_deref(), Some("maksimal 100 MB"));
    assert!(!work.path().join(FILES_DIR).join("big.bin").exists());

    // 2001 files in a folder.
    let many = src.path().join("many");
    std::fs::create_dir_all(&many).unwrap();
    for i in 0..=MAX_FILES {
        std::fs::write(many.join(format!("f{i}.txt")), "x").unwrap();
    }
    let work = tempfile::tempdir().unwrap();
    let e = take(&[r(&many, false)], work.path(), Limits::default()).unwrap_err();
    assert_eq!(e.kind, "snapshot_limit");
    assert_eq!(e.message.as_deref(), Some("maksimal 2000 file dan folder"));
    // Exactly 2000 is fine.
    std::fs::remove_file(many.join("f0.txt")).unwrap();
    let work = tempfile::tempdir().unwrap();
    let snap = take(&[r(&many, false)], work.path(), Limits::default()).unwrap();
    assert_eq!(snap.files, MAX_FILES);

    // Nine folder levels below the attached folder.
    let deep_root = src.path().join("deep");
    let mut d = deep_root.clone();
    for i in 0..9 {
        d = d.join(format!("l{i}"));
    }
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("x.txt"), "x").unwrap();
    let work = tempfile::tempdir().unwrap();
    let e = take(&[r(&deep_root, false)], work.path(), Limits::default()).unwrap_err();
    assert_eq!(e.kind, "snapshot_limit");
}

#[test]
fn f2_empty_folders_count_toward_the_entry_limit() {
    let src = tempfile::tempdir().unwrap();
    // A tree of EMPTY folders: 3 per level, 3 levels deep = 3 + 9 + 27 = 39 sub-folders, no file.
    let root = src.path().join("tree");
    fn grow(dir: &Path, level: usize) {
        if level == 0 {
            return;
        }
        for i in 0..3 {
            let d = dir.join(format!("d{i}"));
            std::fs::create_dir_all(&d).unwrap();
            grow(&d, level - 1);
        }
    }
    grow(&root, 3);
    let small = |n| Limits {
        max_files: n,
        ..Limits::default()
    };
    let work = tempfile::tempdir().unwrap();
    let e = take(&[r(&root, false)], work.path(), small(38)).unwrap_err();
    assert_eq!(e.kind, "snapshot_limit");
    assert_eq!(e.message.as_deref(), Some("maksimal 38 file dan folder"));
    let work = tempfile::tempdir().unwrap();
    let snap = take(&[r(&root, false)], work.path(), small(39)).unwrap();
    assert_eq!((snap.files, snap.dirs), (0, 39));
    // Files and folders share the budget: one extra file tips it over.
    std::fs::write(root.join("x.txt"), "x").unwrap();
    let work = tempfile::tempdir().unwrap();
    assert_eq!(
        take(&[r(&root, false)], work.path(), small(39))
            .unwrap_err()
            .kind,
        "snapshot_limit"
    );
    // With the real limit, 2001 empty sub-folders in one folder are refused.
    let wide = src.path().join("wide");
    for i in 0..=MAX_FILES {
        std::fs::create_dir_all(wide.join(format!("e{i}"))).unwrap();
    }
    let work = tempfile::tempdir().unwrap();
    let e = take(&[r(&wide, false)], work.path(), Limits::default()).unwrap_err();
    assert_eq!(e.message.as_deref(), Some("maksimal 2000 file dan folder"));
}

#[test]
fn top_level_links_are_refused() {
    let src = tempfile::tempdir().unwrap();
    let target = src.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    let link = src.path().join("link");
    let ok = std::process::Command::new("cmd")
        .args(["/c", "mklink", "/J"])
        .arg(&link)
        .arg(&target)
        .output()
        .unwrap()
        .status
        .success();
    assert!(ok);
    let work = tempfile::tempdir().unwrap();
    let e = take(&[r(&link, false)], work.path(), Limits::default()).unwrap_err();
    assert_eq!(e.kind, "attachment_link");
}

fn icacls(target: &Path, args: &[&str]) {
    let out = std::process::Command::new("icacls")
        .arg(target)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "icacls {args:?}: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// `<LOCALAPPDATA>\ferrum-acl-XXXX\a\b\claude.exe` with an explicit clean ACL on the base (this
/// user, SYSTEM, Administrators). `%TEMP%` is not used: on this PC it carries Modify for a Codex
/// sandbox group and another SID, which the ancestor rule (rightly) refuses.
struct AclFixture {
    base: tempfile::TempDir,
    a: std::path::PathBuf,
    b: std::path::PathBuf,
    exe: std::path::PathBuf,
}

fn acl_fixture() -> AclFixture {
    let local = std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA");
    let base = tempfile::Builder::new()
        .prefix("ferrum-acl-")
        .tempdir_in(local)
        .unwrap();
    let me = super::acl::current_user_sid().unwrap();
    icacls(
        base.path(),
        &[
            "/inheritance:r",
            "/grant:r",
            &format!("*{me}:(OI)(CI)F"),
            "*S-1-5-18:(OI)(CI)F",
            "*S-1-5-32-544:(OI)(CI)F",
        ],
    );
    let a = base.path().join("a");
    let b = a.join("b");
    std::fs::create_dir_all(&b).unwrap();
    let exe = b.join("claude.exe");
    std::fs::write(&exe, b"MZ").unwrap();
    AclFixture { base, a, b, exe }
}

#[test]
fn e3_executable_in_a_users_writable_folder_is_refused() {
    let fx = acl_fixture();
    assert_eq!(trusted_location(&fx.exe), Ok(()));
    // BUILTIN\Users "Modify" on the exe's FOLDER only: the folder makes it untrusted.
    icacls(&fx.b, &["/grant", "*S-1-5-32-545:M"]);
    let err = trusted_location(&fx.exe).unwrap_err();
    assert!(err.contains("S-1-5-32-545"), "{err}");
    icacls(&fx.b, &["/remove:g", "*S-1-5-32-545"]);
    assert_eq!(trusted_location(&fx.exe), Ok(()));
    // Everyone "Write" on the file itself.
    icacls(&fx.exe, &["/grant", "*S-1-1-0:W"]);
    assert!(trusted_location(&fx.exe).unwrap_err().contains("S-1-1-0"));
    icacls(&fx.exe, &["/remove:g", "*S-1-1-0"]);
    assert_eq!(trusted_location(&fx.exe), Ok(()));
    let _ = &fx.base;
}

#[test]
fn f1_ancestor_with_delete_child_for_users_is_refused() {
    let fx = acl_fixture();
    // Users may delete children of an ANCESTOR (`a`): they could move `b` away and plant a copy.
    icacls(&fx.a, &["/grant", "*S-1-5-32-545:(DC)"]);
    let err = trusted_location(&fx.exe).unwrap_err();
    assert!(
        err.contains("S-1-5-32-545") && err.contains(r"\a "),
        "{err}"
    );
    icacls(&fx.a, &["/remove:g", "*S-1-5-32-545"]);
    // Create-subfolder below the root is refused as well …
    icacls(&fx.a, &["/grant", "*S-1-5-11:(AD)"]);
    assert!(trusted_location(&fx.exe).unwrap_err().contains("S-1-5-11"));
    icacls(&fx.a, &["/remove:g", "*S-1-5-11"]);
    // … while "create files" alone on an ancestor is harmless.
    icacls(&fx.a, &["/grant", "*S-1-5-32-545:(WD)"]);
    assert_eq!(trusted_location(&fx.exe), Ok(()));
    // An ancestor owned by another account (here: the Users group cannot be set as owner
    // without privileges, so the owner rule is covered by the pure tests in acl_tests.rs).
    let _ = &fx.base;
}

#[test]
fn e3_installed_agent_locations_on_this_pc_pass() {
    // node.exe (Program Files) and, when present, the npm-installed agents.
    let dirs = super::detect::search_dirs(&|k| std::env::var_os(k));
    for agent in [super::AgentId::Claude, super::AgentId::Codex] {
        if let Some(l) = super::detect::resolve(agent, &dirs, &|p| p.is_file()) {
            // ACL check only: the agent itself is never started here.
            assert_eq!(super::detect::launch_trusted(&l), Ok(()), "{agent:?} {l:?}");
        }
    }
    // Program Files (TrustedInstaller-owned) passes as well.
    let pf_node = Path::new(r"C:\Program Files\nodejs\node.exe");
    if pf_node.is_file() {
        assert_eq!(trusted_location(pf_node), Ok(()));
    }
}

// ---------------------------------------------------------------------------------------------
// FER-493: Office companions (extracted text + pictures)
// ---------------------------------------------------------------------------------------------

fn png(n: u8) -> Vec<u8> {
    let mut b = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    b.extend_from_slice(&[n; 24]);
    b
}

fn docx(path: &Path, text: &str, pictures: usize) {
    use std::io::Write;
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let opts = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    zip.start_file("word/document.xml", opts).unwrap();
    write!(
        zip,
        r#"<w:document xmlns:w="w"><w:body><w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>Intro</w:t></w:r></w:p><w:p><w:r><w:t>{text}</w:t></w:r></w:p></w:body></w:document>"#
    )
    .unwrap();
    for i in 1..=pictures {
        zip.start_file(format!("word/media/image{i}.png"), opts)
            .unwrap();
        zip.write_all(&png(i as u8)).unwrap();
    }
    zip.finish().unwrap();
}

#[test]
fn fer493_office_attachment_gets_text_and_pictures_next_to_its_copy() {
    let src = tempfile::tempdir().unwrap();
    let doc = src.path().join("guide.docx");
    docx(&doc, "Hello agent", 3);
    let work = tempfile::tempdir().unwrap();
    let snap = take(&[r(&doc, false)], work.path(), Limits::default()).unwrap();
    let files = work.path().join(FILES_DIR);
    assert!(files.join("guide.docx").is_file());
    assert_eq!(
        std::fs::read_to_string(files.join("guide.docx.txt")).unwrap(),
        "# Intro\nHello agent\n"
    );
    for i in 1..=3u8 {
        assert_eq!(
            std::fs::read(
                files
                    .join("guide.docx_media")
                    .join(format!("image{i:03}.png"))
            )
            .unwrap(),
            png(i)
        );
    }
    let office = snap.items[0].office.as_ref().unwrap();
    assert_eq!(office.text.as_deref(), Some(r"files\guide.docx.txt"));
    assert_eq!(
        office.media,
        Some((r"files\guide.docx_media".to_owned(), 3))
    );
    assert_eq!(office.note, None);
    // Copy + text + 3 pictures, one picture folder: all inside the snapshot budget.
    assert_eq!((snap.files, snap.dirs), (5, 1));
    let doc_len = std::fs::metadata(&doc).unwrap().len();
    assert_eq!(snap.bytes, doc_len + 20 + 3 * 32);
}

#[test]
fn fer493_companions_count_toward_the_snapshot_limits() {
    let src = tempfile::tempdir().unwrap();
    let doc = src.path().join("pics.docx");
    docx(&doc, "x", 3);
    let work = tempfile::tempdir().unwrap();
    let limits = Limits {
        max_files: 4,
        ..Limits::default()
    };
    let e = take(&[r(&doc, false)], work.path(), limits).unwrap_err();
    assert_eq!(e.kind, "snapshot_limit");
}

#[test]
fn fer493_a_secret_in_the_extracted_text_needs_the_tick() {
    let src = tempfile::tempdir().unwrap();
    let doc = src.path().join("keys.docx");
    docx(&doc, "aws AKIAABCDEFGHIJKLMNOP", 0);
    let work = tempfile::tempdir().unwrap();
    let e = take(&[r(&doc, false)], work.path(), Limits::default()).unwrap_err();
    assert_eq!(e.kind, "sensitive_unconfirmed");
    let work = tempfile::tempdir().unwrap();
    let snap = take(&[r(&doc, true)], work.path(), Limits::default()).unwrap();
    let office = snap.items[0].office.as_ref().unwrap();
    assert_eq!(office.text.as_deref(), Some(r"files\keys.docx.txt"));
    assert_eq!(office.media, None);
}

#[test]
fn fer493_a_refused_document_is_still_copied_with_a_note() {
    let src = tempfile::tempdir().unwrap();
    let doc = src.path().join("old.xlsx");
    std::fs::write(&doc, [0xD0, 0xCF, 0x11, 0xE0, 0, 0, 0, 0]).unwrap();
    let work = tempfile::tempdir().unwrap();
    let snap = take(&[r(&doc, false)], work.path(), Limits::default()).unwrap();
    let office = snap.items[0].office.as_ref().unwrap();
    assert_eq!(office.text, None);
    assert!(
        office
            .note
            .as_deref()
            .unwrap()
            .starts_with("text extraction refused: ")
    );
    assert_eq!(snap.files, 1);
    // A non-Office file gets no companion.
    let txt = src.path().join("plain.txt");
    std::fs::write(&txt, "x").unwrap();
    let work = tempfile::tempdir().unwrap();
    let snap = take(&[r(&txt, false)], work.path(), Limits::default()).unwrap();
    assert_eq!(snap.items[0].office, None);
}

#[test]
fn fer493_a_picture_matching_a_secret_is_listed_as_not_copied() {
    use std::io::Write;
    let src = tempfile::tempdir().unwrap();
    let doc = src.path().join("pics.docx");
    {
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&doc).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        zip.start_file("word/document.xml", opts).unwrap();
        zip.write_all(br#"<w:document xmlns:w="w"><w:body><w:p><w:r><w:t>x</w:t></w:r></w:p></w:body></w:document>"#).unwrap();
        zip.start_file("word/media/a.png", opts).unwrap();
        zip.write_all(&png(1)).unwrap();
        zip.start_file("word/media/b.png", opts).unwrap();
        let mut secret = png(2);
        secret.extend_from_slice(b" AKIAABCDEFGHIJKLMNOP ");
        zip.write_all(&secret).unwrap();
        zip.finish().unwrap();
    }
    let work = tempfile::tempdir().unwrap();
    let snap = take(&[r(&doc, false)], work.path(), Limits::default()).unwrap();
    let office = snap.items[0].office.as_ref().unwrap();
    assert_eq!(office.media.as_ref().unwrap().1, 1);
    assert_eq!(snap.skipped.len(), 1);
    assert_eq!(snap.skipped[0].reason, "sensitive");
    assert!(snap.skipped[0].path.ends_with("pics.docx › image002.png"));
}
