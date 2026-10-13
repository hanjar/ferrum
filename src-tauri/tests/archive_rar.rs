//! RAR/RAR5 integration tests against synthetic legal fixtures generated at test time.
//! Fixtures are generated on-the-fly using the tester's `Rar.exe`.
//! If `Rar.exe` is not found, tests requiring it are skipped with an explicit message.

#[path = "../src/archive_contract.rs"]
#[allow(dead_code)]
mod archive_contract;
#[path = "../src/archive_create_test.rs"]
#[allow(clippy::duplicate_mod, dead_code)]
mod archive_create_test;
#[path = "../src/archive_extract.rs"]
#[allow(clippy::duplicate_mod, dead_code)]
mod archive_extract;
#[path = "../src/archive_rar.rs"]
mod archive_rar;
#[path = "../src/archive_publish.rs"]
pub mod publish;

use archive_contract::{
    ArchiveConflictPolicy, ArchiveCreateFormat, ArchiveCreateRequest, ArchiveErrorCode as Code,
    ArchiveExtractRequest, ArchiveLimits, ArchiveListRequest, ArchiveProvider, ArchiveReadRequest,
    Location, MemberPath,
};
use archive_rar::RarProvider;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

const TEST_PASSWORD: &str = "FerrumTest123";

/// Owned temp dir under %TEMP%, removed on Drop (also on assertion failure).
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir().join(format!(
            "ferrum-rartest-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Locate `Rar.exe` in order of precedence:
/// 1. `FERRUM_RAR_EXE` environment variable (if invalid/nonexistent, stops search and returns None)
/// 2. `C:\Program Files\WinRAR\Rar.exe`
/// 3. `C:\Program Files (x86)\WinRAR\Rar.exe`
/// 4. PATH
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

/// Deterministic pseudo-random byte generator (xorshift64*).
struct SeededRng(u64);

impl SeededRng {
    fn new(seed: u64) -> Self {
        let val = seed ^ 0x9E37_79B9_7F4A_7C15;
        Self(if val == 0 { 0x517C_C1B7_2722_0A95 } else { val })
    }

    fn fill(&mut self, buf: &mut [u8]) {
        for b in buf.iter_mut() {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            *b = (x >> 24) as u8;
        }
    }
}

struct RarFixtures {
    basic: PathBuf,
    solid: PathBuf,
    unicode: PathBuf,
    pw_data: PathBuf,
    pw_header: PathBuf,
    stored_big: PathBuf,
    multivol_part1: PathBuf,
    corrupt: PathBuf,

    hash_readme: String,
    hash_angka: String,
    hash_kosong: String,
    hash_catatan: String,
    hash_dalam: String,
    hash_big_4m: String,
}

impl RarFixtures {
    fn generate(rar_exe: &Path) -> Self {
        // Cached in a static OnceLock, so its Drop never runs: keep it under cargo's
        // per-binary tmp dir (target\tmp) instead of %TEMP%. Wipe leftovers from the
        // previous run so the fixtures are always fresh.
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("rar-fixtures-archive_rar");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let src_dir = root.join("src");
        let out_dir = root.join("out");
        std::fs::create_dir_all(&out_dir).unwrap();

        // 1. Synthetic "basic" directory
        let src_basic = src_dir.join("basic");
        std::fs::create_dir_all(src_basic.join("data")).unwrap();
        std::fs::create_dir_all(src_basic.join("docs/deep/a/b")).unwrap();
        std::fs::create_dir_all(src_basic.join("folder-kosong/juga-kosong")).unwrap();

        let readme_bytes =
            b"Ferrum RAR fixture - basic.\r\nIsi sintetis, bebas dipakai untuk tes.\r\n".to_vec();
        std::fs::write(src_basic.join("readme.txt"), &readme_bytes).unwrap();

        let mut angka_bytes = Vec::new();
        for i in 0..500 {
            if i > 0 {
                angka_bytes.push(b'\n');
            }
            angka_bytes.extend_from_slice(format!("{i},{}", i * i).as_bytes());
        }
        std::fs::write(src_basic.join("data/angka.csv"), &angka_bytes).unwrap();

        let kosong_bytes = Vec::new();
        std::fs::write(src_basic.join("data/kosong.txt"), &kosong_bytes).unwrap();

        let catatan_bytes = "Catatan kecil.\n".repeat(20).into_bytes();
        std::fs::write(src_basic.join("docs/catatan.txt"), &catatan_bytes).unwrap();

        let dalam_bytes = b"Folder bertingkat.\n".to_vec();
        std::fs::write(src_basic.join("docs/deep/a/b/dalam.txt"), &dalam_bytes).unwrap();

        let hash_readme = hex::encode(Sha256::digest(&readme_bytes));
        let hash_angka = hex::encode(Sha256::digest(&angka_bytes));
        let hash_kosong = hex::encode(Sha256::digest(&kosong_bytes));
        let hash_catatan = hex::encode(Sha256::digest(&catatan_bytes));
        let hash_dalam = hex::encode(Sha256::digest(&dalam_bytes));

        // 2. Synthetic "unicode" directory
        let src_unicode = src_dir.join("unicode");
        std::fs::create_dir_all(src_unicode.join("emoji ✨")).unwrap();
        std::fs::create_dir_all(src_unicode.join("Ünïcödé")).unwrap();
        std::fs::create_dir_all(src_unicode.join("日本語")).unwrap();

        std::fs::write(
            src_unicode.join("Laporan Keuangan 2026.txt"),
            b"spasi di nama\n",
        )
        .unwrap();
        std::fs::write(
            src_unicode.join("emoji ✨").join("bintang ⭐.txt"),
            b"emoji\n",
        )
        .unwrap();
        std::fs::write(
            src_unicode.join("Ünïcödé").join("café.txt"),
            b"aksen Latin\n",
        )
        .unwrap();
        std::fs::write(
            src_unicode.join("日本語").join("テスト.txt"),
            b"nama Jepang\n",
        )
        .unwrap();

        // 3. Synthetic "medium" directory (512 KiB pseudo-random with fixed seed)
        let src_medium = src_dir.join("medium");
        std::fs::create_dir_all(&src_medium).unwrap();
        let mut medium_bytes = vec![0u8; 512 * 1024];
        SeededRng::new(12345).fill(&mut medium_bytes);
        std::fs::write(src_medium.join("acak-512k.bin"), &medium_bytes).unwrap();

        // 4. Synthetic "big" directory (4 MiB pseudo-random with fixed seed)
        let src_big = src_dir.join("big");
        std::fs::create_dir_all(&src_big).unwrap();
        let mut big_bytes = vec![0u8; 4 * 1024 * 1024];
        SeededRng::new(67890).fill(&mut big_bytes);
        let hash_big_4m = hex::encode(Sha256::digest(&big_bytes));
        std::fs::write(src_big.join("acak-4m.bin"), &big_bytes).unwrap();

        // 5. Run Rar.exe commands to create archives
        let run_rar = |src: &Path, out: &Path, extra: &[&str]| {
            let mut cmd = Command::new(rar_exe);
            cmd.current_dir(src);
            cmd.arg("a");
            cmd.arg("-r");
            cmd.arg("-idq");
            cmd.arg("-ma5");
            for &arg in extra {
                cmd.arg(arg);
            }
            cmd.arg(out);
            cmd.arg("*");

            let output = cmd
                .output()
                .unwrap_or_else(|e| panic!("failed to spawn Rar.exe ({}): {e}", rar_exe.display()));
            if !output.status.success() {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                panic!(
                    "Rar.exe failed with exit code {:?} creating {}\nstdout: {}\nstderr: {}",
                    output.status.code(),
                    out.display(),
                    stdout,
                    stderr
                );
            }
        };

        let basic = out_dir.join("basic-rar5.rar");
        run_rar(&src_basic, &basic, &[]);

        let solid = out_dir.join("solid-rar5.rar");
        run_rar(&src_basic, &solid, &["-s"]);

        let unicode = out_dir.join("unicode-rar5.rar");
        run_rar(&src_unicode, &unicode, &[]);

        let pw_data = out_dir.join("pw-data-rar5.rar");
        let pw_flag = format!("-p{TEST_PASSWORD}");
        run_rar(&src_basic, &pw_data, &[&pw_flag]);

        let pw_header = out_dir.join("pw-header-rar5.rar");
        let hp_flag = format!("-hp{TEST_PASSWORD}");
        run_rar(&src_basic, &pw_header, &[&hp_flag]);

        let stored_big = out_dir.join("stored-big-rar5.rar");
        run_rar(&src_big, &stored_big, &["-m0"]);

        let multivol_base = out_dir.join("multivol-rar5.rar");
        run_rar(&src_medium, &multivol_base, &["-v200k"]);
        let multivol_part1 = out_dir.join("multivol-rar5.part1.rar");
        assert!(
            multivol_part1.is_file(),
            "multivol-rar5.part1.rar was not created by Rar.exe"
        );

        let corrupt = out_dir.join("corrupt-rar5.rar");
        let mut corrupt_bytes = std::fs::read(&basic).expect("read basic-rar5.rar for corrupt");
        let mid = corrupt_bytes.len() / 2;
        for i in 0..16 {
            corrupt_bytes[mid + i] ^= 0xFF;
        }
        std::fs::write(&corrupt, &corrupt_bytes).expect("write corrupt-rar5.rar");

        RarFixtures {
            basic,
            solid,
            unicode,
            pw_data,
            pw_header,
            stored_big,
            multivol_part1,
            corrupt,
            hash_readme,
            hash_angka,
            hash_kosong,
            hash_catatan,
            hash_dalam,
            hash_big_4m,
        }
    }
}

static FIXTURES: OnceLock<Option<RarFixtures>> = OnceLock::new();

fn get_rar_fixtures() -> Option<&'static RarFixtures> {
    FIXTURES
        .get_or_init(|| {
            let rar_exe = find_rar_exe()?;
            Some(RarFixtures::generate(&rar_exe))
        })
        .as_ref()
}

/// After a failed extract nothing may remain under `dest`: no final file and no `.ferrum-partial-` file.
fn assert_no_files(dest: &Path) {
    let files: Vec<PathBuf> = collect_all_files(dest)
        .into_iter()
        .filter(|p| p.is_file())
        .collect();
    assert!(
        files.is_empty(),
        "leftover files after failed extract: {files:?}"
    );
}

fn file_sha256(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap();
    hex::encode(Sha256::digest(&bytes))
}

fn collect_all_files(root: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    if !root.exists() {
        return result;
    }
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.filter_map(|e| e.ok()) {
                let path = entry.path();
                if path.is_dir() {
                    dirs.push(path.clone());
                }
                result.push(path);
            }
        }
    }
    result
}

fn list_req(path: &Path, member: &str) -> ArchiveListRequest {
    ArchiveListRequest {
        location: Location::Archive {
            archive_path: path.to_string_lossy().into_owned(),
            member_path: MemberPath::parse(member).unwrap(),
        },
        request_token: 1,
        password: None,
    }
}

fn default_limits() -> ArchiveLimits {
    ArchiveLimits {
        max_entries: 1000,
        max_total_bytes: 64 << 20,
        max_entry_bytes: 32 << 20,
        max_depth: 16,
    }
}

fn extract_req(
    path: &Path,
    dest: &Path,
    policy: ArchiveConflictPolicy,
    member_paths: Vec<MemberPath>,
    password: Option<String>,
) -> ArchiveExtractRequest {
    ArchiveExtractRequest {
        archive_path: path.to_string_lossy().into_owned(),
        member_paths,
        destination_path: dest.to_string_lossy().into_owned(),
        conflict: policy,
        password,
        limits: default_limits(),
    }
}

#[test]
fn basic_list_root_and_subfolders_including_empty_and_zero_byte() {
    let Some(fixtures) = get_rar_fixtures() else {
        eprintln!(
            "SKIP basic_list_root_and_subfolders_including_empty_and_zero_byte: Rar.exe not found"
        );
        return;
    };
    let path = &fixtures.basic;
    let provider = RarProvider::new();

    // 1. Root listing
    let resp = provider.list(&list_req(path, "")).unwrap();
    let names: Vec<&str> = resp.entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"readme.txt"));
    assert!(names.contains(&"data"));
    assert!(names.contains(&"docs"));
    assert!(names.contains(&"folder-kosong"));

    let readme = resp
        .entries
        .iter()
        .find(|e| e.name == "readme.txt")
        .unwrap();
    assert!(!readme.is_dir);
    assert_eq!(readme.size, 69);

    let data_dir = resp.entries.iter().find(|e| e.name == "data").unwrap();
    assert!(data_dir.is_dir);

    // 2. Subfolder "data" listing (includes 0-byte file)
    let resp_data = provider.list(&list_req(path, "data")).unwrap();
    let data_names: Vec<&str> = resp_data.entries.iter().map(|e| e.name.as_str()).collect();
    assert!(data_names.contains(&"angka.csv"));
    assert!(data_names.contains(&"kosong.txt"));

    let kosong = resp_data
        .entries
        .iter()
        .find(|e| e.name == "kosong.txt")
        .unwrap();
    assert!(!kosong.is_dir);
    assert_eq!(kosong.size, 0);

    let angka = resp_data
        .entries
        .iter()
        .find(|e| e.name == "angka.csv")
        .unwrap();
    assert_eq!(angka.size, 4926);

    // 3. Subfolder "folder-kosong" (empty folder hierarchy)
    let resp_fk = provider.list(&list_req(path, "folder-kosong")).unwrap();
    assert_eq!(resp_fk.entries.len(), 1);
    assert_eq!(resp_fk.entries[0].name, "juga-kosong");
    assert!(resp_fk.entries[0].is_dir);

    // 4. "folder-kosong/juga-kosong" has no children
    let resp_empty = provider
        .list(&list_req(path, "folder-kosong/juga-kosong"))
        .unwrap();
    assert!(resp_empty.entries.is_empty());
}

#[test]
fn basic_extract_full_identical_content() {
    let Some(fixtures) = get_rar_fixtures() else {
        eprintln!("SKIP basic_extract_full_identical_content: Rar.exe not found");
        return;
    };
    let path = &fixtures.basic;
    let dir = TempDir::new("basic-x-full");
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    let provider = RarProvider::new();
    let req = extract_req(path, &dest, ArchiveConflictPolicy::Skip, vec![], None);
    provider.extract(&req, &mut |_| true).unwrap();

    assert_eq!(file_sha256(&dest.join("readme.txt")), fixtures.hash_readme);
    assert_eq!(
        file_sha256(&dest.join("data/angka.csv")),
        fixtures.hash_angka
    );
    assert_eq!(
        file_sha256(&dest.join("data/kosong.txt")),
        fixtures.hash_kosong
    );
    assert_eq!(
        file_sha256(&dest.join("docs/catatan.txt")),
        fixtures.hash_catatan
    );
    assert_eq!(
        file_sha256(&dest.join("docs/deep/a/b/dalam.txt")),
        fixtures.hash_dalam
    );
    assert!(dest.join("folder-kosong/juga-kosong").is_dir());

    // Verify no staging files left behind
    let all = collect_all_files(&dest);
    let partials: Vec<_> = all
        .iter()
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains("ferrum-partial")
        })
        .collect();
    assert!(partials.is_empty(), "staging files left: {partials:?}");
}

#[test]
fn basic_extract_subtree() {
    let Some(fixtures) = get_rar_fixtures() else {
        eprintln!("SKIP basic_extract_subtree: Rar.exe not found");
        return;
    };
    let path = &fixtures.basic;
    let dir = TempDir::new("basic-x-sub");
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    let provider = RarProvider::new();
    let req = extract_req(
        path,
        &dest,
        ArchiveConflictPolicy::Skip,
        vec![MemberPath::parse("docs").unwrap()],
        None,
    );
    provider.extract(&req, &mut |_| true).unwrap();

    assert!(dest.join("docs/catatan.txt").exists());
    assert_eq!(
        file_sha256(&dest.join("docs/catatan.txt")),
        fixtures.hash_catatan
    );
    assert!(dest.join("docs/deep/a/b/dalam.txt").exists());
    assert_eq!(
        file_sha256(&dest.join("docs/deep/a/b/dalam.txt")),
        fixtures.hash_dalam
    );

    // Other top-level items must not be extracted
    assert!(!dest.join("readme.txt").exists());
    assert!(!dest.join("data").exists());
    assert!(!dest.join("folder-kosong").exists());
}

#[test]
fn solid_extract_full_and_read_middle_member() {
    let Some(fixtures) = get_rar_fixtures() else {
        eprintln!("SKIP solid_extract_full_and_read_middle_member: Rar.exe not found");
        return;
    };
    let path = &fixtures.solid;
    let dir = TempDir::new("solid-test");
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    let provider = RarProvider::new();

    // 1. Full extract byte-identical
    let req = extract_req(path, &dest, ArchiveConflictPolicy::Skip, vec![], None);
    provider.extract(&req, &mut |_| true).unwrap();
    assert_eq!(file_sha256(&dest.join("readme.txt")), fixtures.hash_readme);
    assert_eq!(
        file_sha256(&dest.join("data/angka.csv")),
        fixtures.hash_angka
    );
    assert_eq!(
        file_sha256(&dest.join("data/kosong.txt")),
        fixtures.hash_kosong
    );
    assert_eq!(
        file_sha256(&dest.join("docs/catatan.txt")),
        fixtures.hash_catatan
    );
    assert_eq!(
        file_sha256(&dest.join("docs/deep/a/b/dalam.txt")),
        fixtures.hash_dalam
    );

    // 2. Read single middle member
    let mut member_bytes = Vec::new();
    let truncated = provider
        .read(
            &ArchiveReadRequest {
                location: Location::Archive {
                    archive_path: path.to_string_lossy().into_owned(),
                    member_path: MemberPath::parse("docs/deep/a/b/dalam.txt").unwrap(),
                },
                max_bytes: 1024,
                password: None,
            },
            &mut |chunk| {
                member_bytes.extend_from_slice(chunk);
                Ok(())
            },
        )
        .unwrap();
    assert!(!truncated);
    assert_eq!(
        hex::encode(Sha256::digest(&member_bytes)),
        fixtures.hash_dalam
    );
}

#[test]
fn unicode_names_exact_after_list_and_extract() {
    let Some(fixtures) = get_rar_fixtures() else {
        eprintln!("SKIP unicode_names_exact_after_list_and_extract: Rar.exe not found");
        return;
    };
    let path = &fixtures.unicode;
    let dir = TempDir::new("unicode-test");
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    let provider = RarProvider::new();

    // 1. Listing preserves Unicode names
    let root = provider.list(&list_req(path, "")).unwrap();
    let root_names: Vec<&str> = root.entries.iter().map(|e| e.name.as_str()).collect();
    assert!(
        root_names.contains(&"Laporan Keuangan 2026.txt"),
        "names: {root_names:?}"
    );

    // 2. Full extract preserves exact UTF-8 filenames and content
    let req = extract_req(path, &dest, ArchiveConflictPolicy::Skip, vec![], None);
    provider.extract(&req, &mut |_| true).unwrap();

    let f1 = dest.join("Laporan Keuangan 2026.txt");
    assert!(f1.exists(), "Laporan Keuangan 2026.txt exists");
    assert_eq!(std::fs::read(&f1).unwrap(), b"spasi di nama\n");

    let f2 = dest.join("emoji ✨/bintang ⭐.txt");
    assert!(f2.exists(), "emoji path exists");
    assert_eq!(std::fs::read(&f2).unwrap(), b"emoji\n");

    let f3 = dest.join("Ünïcödé/café.txt");
    assert!(f3.exists(), "accented path exists");
    assert_eq!(std::fs::read(&f3).unwrap(), b"aksen Latin\n");

    let f4 = dest.join("日本語/テスト.txt");
    assert!(f4.exists(), "Japanese path exists");
    assert_eq!(std::fs::read(&f4).unwrap(), b"nama Jepang\n");
}

#[test]
fn pw_data_matrix() {
    let Some(fixtures) = get_rar_fixtures() else {
        eprintln!("SKIP pw_data_matrix: Rar.exe not found");
        return;
    };
    let path = &fixtures.pw_data;
    let dir = TempDir::new("pw-data-test");
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    let provider = RarProvider::new();

    // 1. List without password succeeds (data encrypted, headers plaintext)
    let resp = provider.list(&list_req(path, "")).unwrap();
    assert!(!resp.entries.is_empty());
    let names: Vec<&str> = resp.entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"readme.txt"));

    // 2. Extract without password yields PasswordRequired
    let req_no_pw = extract_req(path, &dest, ArchiveConflictPolicy::Skip, vec![], None);
    let err = provider.extract(&req_no_pw, &mut |_| true).unwrap_err();
    assert_eq!(err.code, Code::PasswordRequired);
    assert_no_files(&dest);

    // 3. Extract with wrong password yields BadPassword
    let req_wrong = extract_req(
        path,
        &dest,
        ArchiveConflictPolicy::Skip,
        vec![],
        Some("WrongPassword123".to_owned()),
    );
    let err = provider.extract(&req_wrong, &mut |_| true).unwrap_err();
    assert_eq!(err.code, Code::BadPassword);
    assert_no_files(&dest);

    // 4. Extract with correct password gives identical content
    let req_correct = extract_req(
        path,
        &dest,
        ArchiveConflictPolicy::Skip,
        vec![],
        Some(TEST_PASSWORD.to_owned()),
    );
    provider.extract(&req_correct, &mut |_| true).unwrap();
    assert_eq!(file_sha256(&dest.join("readme.txt")), fixtures.hash_readme);
    assert_eq!(
        file_sha256(&dest.join("data/angka.csv")),
        fixtures.hash_angka
    );
}

#[test]
fn pw_data_read_with_password_returns_member_bytes() {
    let Some(fixtures) = get_rar_fixtures() else {
        eprintln!("SKIP pw_data_read_with_password_returns_member_bytes: Rar.exe not found");
        return;
    };
    let path = &fixtures.pw_data;
    let provider = RarProvider::new();

    let mut bytes = Vec::new();
    provider
        .read(
            &ArchiveReadRequest {
                location: Location::Archive {
                    archive_path: path.to_string_lossy().into_owned(),
                    member_path: MemberPath::parse("readme.txt").unwrap(),
                },
                max_bytes: 1 << 20,
                password: Some(TEST_PASSWORD.to_owned()),
            },
            &mut |chunk| {
                bytes.extend_from_slice(chunk);
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(hex::encode(Sha256::digest(&bytes)), fixtures.hash_readme);
}

#[test]
fn pw_header_matrix() {
    let Some(fixtures) = get_rar_fixtures() else {
        eprintln!("SKIP pw_header_matrix: Rar.exe not found");
        return;
    };
    let path = &fixtures.pw_header;
    let provider = RarProvider::new();

    // 1. List without password yields PasswordRequired
    let err = provider.list(&list_req(path, "")).unwrap_err();
    assert_eq!(err.code, Code::PasswordRequired);

    // 2. List with correct password succeeds via the request field (FER-504,
    //    the app path; `list_with_password` stays as the engine seam)
    let mut req = list_req(path, "");
    req.password = Some(TEST_PASSWORD.to_owned());
    let resp = provider.list(&req).unwrap();
    assert!(!resp.entries.is_empty());
    let names: Vec<&str> = resp.entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"readme.txt"));

    // 2b. The password applies to subfolder listings too.
    let mut sub = list_req(path, "data");
    sub.password = Some(TEST_PASSWORD.to_owned());
    let resp = provider.list(&sub).unwrap();
    let names: Vec<&str> = resp.entries.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"angka.csv"));

    // 3. Wrong password yields BadPassword, not PasswordRequired, so the UI
    //    can re-prompt with the "wrong password" message.
    let mut wrong = list_req(path, "");
    wrong.password = Some("WrongPassword123".to_owned());
    let err = provider.list(&wrong).unwrap_err();
    assert_eq!(err.code, Code::BadPassword);
}

#[test]
fn stored_big_bounded_read_and_full_extract() {
    let Some(fixtures) = get_rar_fixtures() else {
        eprintln!("SKIP stored_big_bounded_read_and_full_extract: Rar.exe not found");
        return;
    };
    let path = &fixtures.stored_big;
    let provider = RarProvider::new();

    // 1. Bounded read where unpacked_size (4 MiB) exceeds max_bytes (1000) errors with LimitExceeded
    let err = provider
        .read(
            &ArchiveReadRequest {
                location: Location::Archive {
                    archive_path: path.to_string_lossy().into_owned(),
                    member_path: MemberPath::parse("acak-4m.bin").unwrap(),
                },
                max_bytes: 1000,
                password: None,
            },
            &mut |_| Ok(()),
        )
        .unwrap_err();
    assert_eq!(err.code, Code::LimitExceeded);

    // 2. Full extract matches expected SHA-256 and size (4,194,304 bytes)
    let dir = TempDir::new("stored-big-x");
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    let req = extract_req(path, &dest, ArchiveConflictPolicy::Skip, vec![], None);
    provider.extract(&req, &mut |_| true).unwrap();

    let big_file = dest.join("acak-4m.bin");
    assert!(big_file.exists());
    assert_eq!(std::fs::metadata(&big_file).unwrap().len(), 4_194_304);
    assert_eq!(file_sha256(&big_file), fixtures.hash_big_4m);
}

#[test]
fn stored_big_cancel_mid_extract_leaves_no_partial_files() {
    let Some(fixtures) = get_rar_fixtures() else {
        eprintln!("SKIP stored_big_cancel_mid_extract_leaves_no_partial_files: Rar.exe not found");
        return;
    };
    let path = &fixtures.stored_big;
    let dir = TempDir::new("stored-big-cancel");
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    let provider = RarProvider::new();
    let req = extract_req(path, &dest, ArchiveConflictPolicy::Skip, vec![], None);

    // Cancel after receiving >= 128 KiB
    let mut saw_progress = false;
    let result = provider.extract(&req, &mut |prog| {
        saw_progress = true;
        prog.processed_bytes < 128 * 1024
    });

    let err = result.unwrap_err();
    assert_eq!(err.code, Code::Cancelled);
    assert!(saw_progress);

    // Verify: no completed target file and no partial staging files
    assert!(
        !dest.join("acak-4m.bin").exists(),
        "cancelled file must not be committed to destination"
    );

    let all = collect_all_files(&dest);
    let partials: Vec<_> = all
        .iter()
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains("ferrum-partial")
        })
        .collect();
    assert!(
        partials.is_empty(),
        "partial staging files left: {partials:?}"
    );
}

#[test]
fn multivol_part1_rejected_without_panic_or_leftover() {
    let Some(fixtures) = get_rar_fixtures() else {
        eprintln!("SKIP multivol_part1_rejected_without_panic_or_leftover: Rar.exe not found");
        return;
    };
    let path = &fixtures.multivol_part1;
    let provider = RarProvider::new();

    // 1. Listing part1 is rejected with "Multi-volume RAR not supported"
    let list_err = provider.list(&list_req(path, "")).unwrap_err();
    assert_eq!(list_err.code, Code::UnsupportedCodec);
    assert!(
        list_err.message.contains("Multi-volume RAR not supported"),
        "message: {}",
        list_err.message
    );

    // 2. Extracting part1 is rejected without leaving files
    let dir = TempDir::new("multivol-x");
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    let extract_err = provider
        .extract(
            &extract_req(path, &dest, ArchiveConflictPolicy::Skip, vec![], None),
            &mut |_| true,
        )
        .unwrap_err();
    assert_eq!(extract_err.code, Code::UnsupportedCodec);
    assert!(
        extract_err
            .message
            .contains("Multi-volume RAR not supported"),
        "message: {}",
        extract_err.message
    );

    let entries: Vec<_> = std::fs::read_dir(&dest).unwrap().collect();
    assert!(entries.is_empty(), "dest should be empty: {entries:?}");
}

#[test]
fn corrupt_archive_fails_with_structured_error_and_no_partial_files() {
    let Some(fixtures) = get_rar_fixtures() else {
        eprintln!(
            "SKIP corrupt_archive_fails_with_structured_error_and_no_partial_files: Rar.exe not found"
        );
        return;
    };
    let path = &fixtures.corrupt;
    let provider = RarProvider::new();

    let dir = TempDir::new("corrupt-test");
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    // Listing or extraction fails with CorruptArchive
    let extract_err = provider
        .extract(
            &extract_req(path, &dest, ArchiveConflictPolicy::Skip, vec![], None),
            &mut |_| true,
        )
        .unwrap_err();
    assert_eq!(extract_err.code, Code::CorruptArchive);

    // No leftover files in destination
    let all = collect_all_files(&dest);
    let partials: Vec<_> = all
        .iter()
        .filter(|p| {
            p.file_name()
                .unwrap()
                .to_string_lossy()
                .contains("ferrum-partial")
        })
        .collect();
    assert!(partials.is_empty(), "leftovers: {partials:?}");
}

#[test]
fn test_rar_basic_passes() {
    let Some(fixtures) = get_rar_fixtures() else {
        eprintln!("SKIP test_rar_basic_passes: Rar.exe not found");
        return;
    };
    let report = archive_create_test::test_rar(&fixtures.basic, None).unwrap();
    assert!(!report.entries.is_empty());
    assert!(
        report
            .entries
            .iter()
            .all(|e| e.status == archive_contract::ArchiveTestStatus::Passed)
    );
}

#[test]
fn test_rar_corrupt_is_detected() {
    let Some(fixtures) = get_rar_fixtures() else {
        eprintln!("SKIP test_rar_corrupt_is_detected: Rar.exe not found");
        return;
    };
    let path = &fixtures.corrupt;
    match archive_create_test::test_rar(path, None) {
        Err(_) => {}
        Ok(report) => assert!(
            report
                .entries
                .iter()
                .any(|e| e.status != archive_contract::ArchiveTestStatus::Passed),
            "corrupt archive reported every member as Passed"
        ),
    }
}

#[test]
fn create_rar_is_unsupported() {
    let provider = RarProvider::new();
    let req = ArchiveCreateRequest {
        format: ArchiveCreateFormat::Zip, // even if caller asks, provider enforces RAR
        source_paths: vec!["some/path".into()],
        destination_path: "C:/out.rar".into(),
        conflict: ArchiveConflictPolicy::Skip,
        level: None,
    };
    let err = provider.create(&req, &mut |_| true).unwrap_err();
    assert_eq!(err.code, Code::UnsupportedFormat);
    assert!(err.message.contains("RAR creation is unsupported"));
}

#[test]
fn extract_conflict_skip_preserves_existing_target() {
    let Some(fixtures) = get_rar_fixtures() else {
        eprintln!("SKIP extract_conflict_skip_preserves_existing_target: Rar.exe not found");
        return;
    };
    let path = &fixtures.basic;
    let dir = TempDir::new("conflict-skip");
    let dest = dir.path().join("out");
    std::fs::create_dir_all(&dest).unwrap();

    let target_file = dest.join("readme.txt");
    std::fs::write(&target_file, b"PRE_EXISTING_CONTENT").unwrap();

    let provider = RarProvider::new();
    let req = extract_req(path, &dest, ArchiveConflictPolicy::Skip, vec![], None);
    provider.extract(&req, &mut |_| true).unwrap();

    assert_eq!(
        std::fs::read(&target_file).unwrap(),
        b"PRE_EXISTING_CONTENT"
    );
}

#[test]
fn non_rar_signature_is_unsupported() {
    let dir = TempDir::new("bad-sig");
    let fake = dir.path().join("fake.rar");
    std::fs::write(&fake, b"this is not a rar archive").unwrap();

    let provider = RarProvider::new();
    let err = provider.list(&list_req(&fake, "")).unwrap_err();
    assert_eq!(err.code, Code::UnsupportedFormat);
}
