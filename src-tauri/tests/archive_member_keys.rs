//! FER-449/451 cross-provider contract: whatever the creating tool wrote
//! (explicit folder entries, implicit parents, or both), `list` returns each
//! member path once (the UI keys rows by member path) and `extract` publishes
//! folders and files byte for byte.

#[path = "../src/archive_contract.rs"]
#[allow(dead_code)]
mod archive_contract;
#[allow(clippy::duplicate_mod, dead_code)]
#[path = "../src/archive_create_test.rs"]
mod archive_create_test;
#[allow(clippy::duplicate_mod, dead_code)]
#[path = "../src/archive_extract.rs"]
mod archive_extract;
#[path = "../src/archive_rar.rs"]
mod archive_rar;
#[path = "../src/archive_sevenz.rs"]
mod archive_sevenz;
#[path = "../src/archive_zip.rs"]
mod archive_zip;
#[path = "../src/archive_publish.rs"]
pub mod publish;

use archive_contract::{
    ArchiveConflictPolicy, ArchiveExtractRequest, ArchiveLimits, ArchiveListRequest,
    ArchiveProvider, Location, MemberPath,
};
use archive_rar::RarProvider;
use archive_sevenz::SevenZProvider;
use archive_zip::ZipProvider;

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Owned temp dir under %TEMP%, removed on Drop (also on assertion failure).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-keys-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    #[allow(dead_code)]
    fn path(&self) -> &Path {
        &self.0
    }
}

impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn temp(tag: &str) -> TempDir {
    TempDir::new(tag)
}

const NOTES: &[u8] = b"hello notes\r\nsecond line\n";
const DEEP: &[u8] = &[0, 1, 2, 3, 255, 254];

/// `docs/` (explicit) + `docs/notes.txt` + `docs/sub/` (explicit) +
/// `docs/sub/deep.bin` + `top.txt`, the shape 7-Zip and Explorer write.
fn write_zip_explicit(path: &Path) {
    let mut zip = zip::ZipWriter::new(std::fs::File::create(path).unwrap());
    let opts = zip::write::SimpleFileOptions::default();
    zip.add_directory("docs/", opts).unwrap();
    zip.start_file("docs/notes.txt", opts).unwrap();
    zip.write_all(NOTES).unwrap();
    zip.add_directory("docs/sub/", opts).unwrap();
    zip.start_file("docs/sub/deep.bin", opts).unwrap();
    zip.write_all(DEEP).unwrap();
    zip.start_file("top.txt", opts).unwrap();
    zip.write_all(b"top").unwrap();
    zip.finish().unwrap();
}

fn write_7z_explicit(path: &Path) {
    use sevenz_rust2::{ArchiveEntry, ArchiveWriter};
    let mut w = ArchiveWriter::new(std::fs::File::create(path).unwrap()).unwrap();
    let none: Option<&[u8]> = None;
    w.push_archive_entry(ArchiveEntry::new_directory("docs"), none)
        .unwrap();
    w.push_archive_entry(ArchiveEntry::new_file("docs/notes.txt"), Some(NOTES))
        .unwrap();
    w.push_archive_entry(ArchiveEntry::new_directory("docs/sub"), none)
        .unwrap();
    w.push_archive_entry(ArchiveEntry::new_file("docs/sub/deep.bin"), Some(DEEP))
        .unwrap();
    w.push_archive_entry(ArchiveEntry::new_file("top.txt"), Some(&b"top"[..]))
        .unwrap();
    w.finish().unwrap();
}

fn list(provider: &dyn ArchiveProvider, archive: &Path, member: &str) -> Vec<(String, bool)> {
    let response = provider
        .list(&ArchiveListRequest {
            location: Location::Archive {
                archive_path: archive.to_string_lossy().into_owned(),
                member_path: MemberPath::parse(member).unwrap(),
            },
            request_token: 1,
            password: None,
        })
        .unwrap();
    response
        .entries
        .iter()
        .map(|e| (e.member_path.as_str().to_owned(), e.is_dir))
        .collect()
}

fn assert_unique_keys(label: &str, entries: &[(String, bool)]) {
    let mut seen = HashSet::new();
    for (key, _) in entries {
        assert!(seen.insert(key), "{label}: duplicate member key {key:?}");
    }
}

fn limits() -> ArchiveLimits {
    ArchiveLimits {
        max_entries: 100,
        max_total_bytes: 16 << 20,
        max_entry_bytes: 8 << 20,
        max_depth: 8,
    }
}

fn extract_all(provider: &dyn ArchiveProvider, archive: &Path, dest: &Path) {
    std::fs::create_dir_all(dest).unwrap();
    provider
        .extract(
            &ArchiveExtractRequest {
                archive_path: archive.to_string_lossy().into_owned(),
                member_paths: vec![],
                destination_path: dest.to_string_lossy().into_owned(),
                conflict: ArchiveConflictPolicy::Overwrite,
                password: None,
                limits: limits(),
            },
            &mut |_| true,
        )
        .unwrap();
}

fn check_explicit_folder_contract(label: &str, provider: &dyn ArchiveProvider, archive: &Path) {
    let root = list(provider, archive, "");
    assert_unique_keys(label, &root);
    assert_eq!(
        root,
        vec![("docs".to_owned(), true), ("top.txt".to_owned(), false)],
        "{label}: root listing"
    );
    let docs = list(provider, archive, "docs");
    assert_unique_keys(label, &docs);
    assert_eq!(
        docs,
        vec![
            ("docs/sub".to_owned(), true),
            ("docs/notes.txt".to_owned(), false)
        ],
        "{label}: docs listing"
    );

    let dest_dir = temp(label);
    let dest = dest_dir.join("out");
    extract_all(provider, archive, &dest);
    assert_eq!(std::fs::read(dest.join("docs/notes.txt")).unwrap(), NOTES);
    assert_eq!(std::fs::read(dest.join("docs/sub/deep.bin")).unwrap(), DEEP);
    assert_eq!(std::fs::read(dest.join("top.txt")).unwrap(), b"top");
    assert!(dest.join("docs/sub").is_dir(), "{label}: folder published");
}

#[test]
fn zip_explicit_folder_entries_list_once_and_extract_byte_exact() {
    let tmp = temp("zip");
    let archive = tmp.join("explicit.zip");
    write_zip_explicit(&archive);
    check_explicit_folder_contract("zip", &ZipProvider::new(), &archive);
}

#[test]
fn sevenz_explicit_folder_entries_list_once_and_extract_byte_exact() {
    let tmp = temp("7z");
    let archive = tmp.join("explicit.7z");
    write_7z_explicit(&archive);
    check_explicit_folder_contract("7z", &SevenZProvider::new(), &archive);
}

#[test]
fn zip_traversal_directory_entries_stay_rejected() {
    for name in ["../evil/", "a/../b/", "/abs/", "C:/x/", "a\\b/", "//"] {
        let dir = temp("zip-bad");
        let archive = dir.join("bad.zip");
        let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).unwrap());
        let opts = zip::write::SimpleFileOptions::default();
        // `add_directory` sanitizes nothing; the raw name reaches the central directory.
        zip.add_directory(name, opts).unwrap();
        zip.finish().unwrap();
        let dest = dir.join("out");
        std::fs::create_dir_all(&dest).unwrap();
        let result = ZipProvider::new().extract(
            &ArchiveExtractRequest {
                archive_path: archive.to_string_lossy().into_owned(),
                member_paths: vec![],
                destination_path: dest.to_string_lossy().into_owned(),
                conflict: ArchiveConflictPolicy::Overwrite,
                password: None,
                limits: limits(),
            },
            &mut |_| true,
        );
        assert!(result.is_err(), "{name:?} must be rejected");
        assert!(
            !dir.join("evil").exists() && !dir.join("b").exists(),
            "{name:?} must not write outside the destination"
        );
    }
}

fn find_rar_exe() -> Option<PathBuf> {
    if let Some(val) = std::env::var_os("FERRUM_RAR_EXE") {
        let p = PathBuf::from(val);
        if p.is_file() {
            return Some(p);
        } else {
            return None;
        }
    }

    let p1 = PathBuf::from(r"C:\Program Files\WinRAR\Rar.exe");
    if p1.is_file() {
        return Some(p1);
    }

    let p2 = PathBuf::from(r"C:\Program Files (x86)\WinRAR\Rar.exe");
    if p2.is_file() {
        return Some(p2);
    }

    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|dir| dir.join("Rar.exe"))
            .find(|p| p.is_file())
    })
}

fn make_basic_rar(tmp: &Path, rar_exe: &Path) -> PathBuf {
    let src = tmp.join("src");
    let out = tmp.join("basic-rar5.rar");
    std::fs::create_dir_all(src.join("data")).unwrap();
    std::fs::create_dir_all(src.join("docs/deep/a/b")).unwrap();
    std::fs::create_dir_all(src.join("folder-kosong/juga-kosong")).unwrap();

    std::fs::write(
        src.join("readme.txt"),
        b"Ferrum RAR fixture - basic.\r\nIsi sintetis, bebas dipakai untuk tes.\r\n",
    )
    .unwrap();

    let mut angka = Vec::new();
    for i in 0..500 {
        if i > 0 {
            angka.push(b'\n');
        }
        angka.extend_from_slice(format!("{i},{}", i * i).as_bytes());
    }
    std::fs::write(src.join("data/angka.csv"), &angka).unwrap();
    std::fs::write(src.join("data/kosong.txt"), b"").unwrap();
    std::fs::write(
        src.join("docs/catatan.txt"),
        "Catatan kecil.\n".repeat(20).as_bytes(),
    )
    .unwrap();
    std::fs::write(src.join("docs/deep/a/b/dalam.txt"), b"Folder bertingkat.\n").unwrap();

    let status = std::process::Command::new(rar_exe)
        .current_dir(&src)
        .args(["a", "-r", "-idq", "-ma5"])
        .arg(&out)
        .arg("*")
        .status()
        .expect("failed to spawn Rar.exe");
    if !status.success() {
        panic!(
            "Rar.exe failed with exit code {:?} creating basic-rar5.rar",
            status.code()
        );
    }
    out
}

#[test]
fn rar_fixture_listing_keys_are_unique() {
    let Some(rar_exe) = find_rar_exe() else {
        eprintln!("SKIP rar_fixture_listing_keys_are_unique: Rar.exe not found");
        return;
    };
    let dir = temp("rar-keys");
    let fixture = make_basic_rar(&dir, &rar_exe);
    assert_unique_keys("rar", &list(&RarProvider::new(), &fixture, ""));
}
