//! FER-58, FER-59, FER-66: Tests for archive compression levels, 7z memory limits, and mtime preservation.

#[path = "../src/archive_contract.rs"]
#[allow(dead_code)]
mod archive_contract;
#[allow(clippy::duplicate_mod, dead_code)]
#[path = "../src/archive_create_test.rs"]
mod archive_create_test;
#[allow(clippy::duplicate_mod, dead_code)]
#[path = "../src/archive_extract.rs"]
mod archive_extract;
#[path = "../src/archive_sevenz.rs"]
#[allow(dead_code)]
mod archive_sevenz;
#[path = "../src/archive_zip.rs"]
#[allow(dead_code)]
mod archive_zip;
#[path = "../src/archive_publish.rs"]
pub mod publish;

use archive_contract::{
    ArchiveCompressionLevel, ArchiveConflictPolicy, ArchiveCreateFormat, ArchiveCreateRequest,
    ArchiveExtractRequest, ArchiveLimits, ArchiveListRequest, ArchiveProvider, Location,
    MemberPath,
};
use archive_sevenz::SevenZProvider;
use archive_zip::ZipProvider;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static SEQ: AtomicU64 = AtomicU64::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-pack-opt-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn limits() -> ArchiveLimits {
    ArchiveLimits {
        max_entries: 100_000,
        max_total_bytes: u64::MAX,
        max_entry_bytes: 512 * 1024 * 1024,
        max_depth: 32,
    }
}

fn create_req(
    format: ArchiveCreateFormat,
    sources: Vec<String>,
    dest: &Path,
    level: Option<ArchiveCompressionLevel>,
) -> ArchiveCreateRequest {
    ArchiveCreateRequest {
        format,
        source_paths: sources,
        destination_path: dest.to_string_lossy().into_owned(),
        conflict: ArchiveConflictPolicy::Overwrite,
        level,
    }
}

#[test]
fn test_store_level_uses_stored_and_copy() {
    let tmp = TempDir::new("store");
    let src = tmp.path().join("sample.txt");
    fs::write(&src, b"Ferrum store level uncompressed data test content.").unwrap();

    // 1. ZIP Store level
    let zip_dest = tmp.path().join("store.zip");
    let zip_req = create_req(
        ArchiveCreateFormat::Zip,
        vec![src.to_string_lossy().into_owned()],
        &zip_dest,
        Some(ArchiveCompressionLevel::Store),
    );
    let provider = ZipProvider::new();
    let res = provider.create(&zip_req, &mut |_| true);
    assert!(res.is_ok(), "create zip store failed: {:?}", res);

    let zip_file = File::open(&zip_dest).unwrap();
    let mut zip_arch = zip::ZipArchive::new(zip_file).unwrap();
    assert_eq!(zip_arch.len(), 1);
    let entry = zip_arch.by_index(0).unwrap();
    assert_eq!(
        entry.compression(),
        zip::CompressionMethod::Stored,
        "ZIP entry must be Stored"
    );

    // 2. 7z Store level (COPY method)
    let sevenz_dest = tmp.path().join("store.7z");
    let sevenz_req = create_req(
        ArchiveCreateFormat::Sevenz,
        vec![src.to_string_lossy().into_owned()],
        &sevenz_dest,
        Some(ArchiveCompressionLevel::Store),
    );
    let provider7 = SevenZProvider::new();
    let res7 = provider7.create(&sevenz_req, &mut |_| true);
    assert!(res7.is_ok(), "create 7z store failed: {:?}", res7);

    let r7 =
        sevenz_rust2::ArchiveReader::open(&sevenz_dest, sevenz_rust2::Password::empty()).unwrap();
    let arch = r7.archive();
    assert_eq!(arch.files.len(), 1);
    assert!(!arch.blocks.is_empty(), "7z archive must contain blocks");
    let is_copy = arch.blocks[0]
        .coders
        .iter()
        .any(|c| c.encoder_method_id() == [0x00]);
    assert!(is_copy, "7z block coder must be COPY (0x00)");
}

#[test]
fn test_max_level_smaller_or_equal_to_fast() {
    let tmp = TempDir::new("compressibility");
    let src = tmp.path().join("text.txt");
    // Repeating text is highly compressible
    let text = "Ferrum dual-pane file manager high compression test line.\n".repeat(4000);
    fs::write(&src, text.as_bytes()).unwrap();

    let provider_zip = ZipProvider::new();
    let provider_7z = SevenZProvider::new();

    // ZIP: Fast vs Max
    let zip_fast = tmp.path().join("fast.zip");
    let zip_max = tmp.path().join("max.zip");
    provider_zip
        .create(
            &create_req(
                ArchiveCreateFormat::Zip,
                vec![src.to_string_lossy().into_owned()],
                &zip_fast,
                Some(ArchiveCompressionLevel::Fast),
            ),
            &mut |_| true,
        )
        .unwrap();
    provider_zip
        .create(
            &create_req(
                ArchiveCreateFormat::Zip,
                vec![src.to_string_lossy().into_owned()],
                &zip_max,
                Some(ArchiveCompressionLevel::Max),
            ),
            &mut |_| true,
        )
        .unwrap();

    let zip_fast_size = fs::metadata(&zip_fast).unwrap().len();
    let zip_max_size = fs::metadata(&zip_max).unwrap().len();
    assert!(
        zip_max_size <= zip_fast_size,
        "ZIP max size ({zip_max_size}) must be <= fast size ({zip_fast_size})"
    );

    // 7z: Fast vs Max
    let sevenz_fast = tmp.path().join("fast.7z");
    let sevenz_max = tmp.path().join("max.7z");
    provider_7z
        .create(
            &create_req(
                ArchiveCreateFormat::Sevenz,
                vec![src.to_string_lossy().into_owned()],
                &sevenz_fast,
                Some(ArchiveCompressionLevel::Fast),
            ),
            &mut |_| true,
        )
        .unwrap();
    provider_7z
        .create(
            &create_req(
                ArchiveCreateFormat::Sevenz,
                vec![src.to_string_lossy().into_owned()],
                &sevenz_max,
                Some(ArchiveCompressionLevel::Max),
            ),
            &mut |_| true,
        )
        .unwrap();

    let sevenz_fast_size = fs::metadata(&sevenz_fast).unwrap().len();
    let sevenz_max_size = fs::metadata(&sevenz_max).unwrap().len();
    assert!(
        sevenz_max_size <= sevenz_fast_size,
        "7z max size ({sevenz_max_size}) must be <= fast size ({sevenz_fast_size})"
    );
}

#[test]
fn test_all_levels_roundtrip() {
    let tmp = TempDir::new("levels-roundtrip");
    let src_dir = tmp.path().join("source");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(src_dir.join("a.txt"), b"File A contents").unwrap();
    fs::write(src_dir.join("b.bin"), vec![0xAB; 2048]).unwrap();
    let sub = src_dir.join("sub");
    fs::create_dir_all(&sub).unwrap();
    fs::write(sub.join("nested.txt"), b"Nested content in sub folder").unwrap();

    let levels = [
        ArchiveCompressionLevel::Store,
        ArchiveCompressionLevel::Fast,
        ArchiveCompressionLevel::Normal,
        ArchiveCompressionLevel::Max,
    ];

    let zip_prov = ZipProvider::new();
    let sevenz_prov = SevenZProvider::new();

    for level in levels {
        // ZIP
        let zip_dest = tmp.path().join(format!("test-{:?}.zip", level));
        let zip_out = zip_prov
            .create(
                &create_req(
                    ArchiveCreateFormat::Zip,
                    vec![src_dir.to_string_lossy().into_owned()],
                    &zip_dest,
                    Some(level),
                ),
                &mut |_| true,
            )
            .unwrap();
        assert!(zip_out.exists());

        let extract_dir_zip = tmp.path().join(format!("extract-zip-{:?}", level));
        fs::create_dir_all(&extract_dir_zip).unwrap();
        zip_prov
            .extract(
                &ArchiveExtractRequest {
                    archive_path: zip_dest.to_string_lossy().into_owned(),
                    member_paths: vec![],
                    destination_path: extract_dir_zip.to_string_lossy().into_owned(),
                    conflict: ArchiveConflictPolicy::Overwrite,
                    password: None,
                    limits: limits(),
                },
                &mut |_| true,
            )
            .unwrap();

        assert_eq!(
            fs::read(extract_dir_zip.join("source/a.txt")).unwrap(),
            b"File A contents"
        );
        assert_eq!(
            fs::read(extract_dir_zip.join("source/b.bin")).unwrap(),
            vec![0xAB; 2048]
        );
        assert_eq!(
            fs::read(extract_dir_zip.join("source/sub/nested.txt")).unwrap(),
            b"Nested content in sub folder"
        );

        // 7z
        let sevenz_dest = tmp.path().join(format!("test-{:?}.7z", level));
        let sevenz_out = sevenz_prov
            .create(
                &create_req(
                    ArchiveCreateFormat::Sevenz,
                    vec![src_dir.to_string_lossy().into_owned()],
                    &sevenz_dest,
                    Some(level),
                ),
                &mut |_| true,
            )
            .unwrap();
        assert!(sevenz_out.exists());

        let extract_dir_7z = tmp.path().join(format!("extract-7z-{:?}", level));
        fs::create_dir_all(&extract_dir_7z).unwrap();
        sevenz_prov
            .extract(
                &ArchiveExtractRequest {
                    archive_path: sevenz_dest.to_string_lossy().into_owned(),
                    member_paths: vec![],
                    destination_path: extract_dir_7z.to_string_lossy().into_owned(),
                    conflict: ArchiveConflictPolicy::Overwrite,
                    password: None,
                    limits: limits(),
                },
                &mut |_| true,
            )
            .unwrap();

        assert_eq!(
            fs::read(extract_dir_7z.join("source/a.txt")).unwrap(),
            b"File A contents"
        );
        assert_eq!(
            fs::read(extract_dir_7z.join("source/b.bin")).unwrap(),
            vec![0xAB; 2048]
        );
        assert_eq!(
            fs::read(extract_dir_7z.join("source/sub/nested.txt")).unwrap(),
            b"Nested content in sub folder"
        );
    }
}

#[test]
fn test_mtime_preserved_on_roundtrip() {
    let tmp = TempDir::new("mtime");
    let src = tmp.path().join("timed.txt");
    fs::write(&src, b"File with timestamp for mtime test").unwrap();

    let disk_meta = fs::metadata(&src).unwrap();
    let disk_mtime = disk_meta.modified().unwrap();
    let disk_ms = disk_mtime
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;

    // 1. ZIP mtime (FER-58)
    let zip_dest = tmp.path().join("mtime.zip");
    let zip_prov = ZipProvider::new();
    zip_prov
        .create(
            &create_req(
                ArchiveCreateFormat::Zip,
                vec![src.to_string_lossy().into_owned()],
                &zip_dest,
                Some(ArchiveCompressionLevel::Normal),
            ),
            &mut |_| true,
        )
        .unwrap();

    let list_req_zip = ArchiveListRequest {
        location: Location::Archive {
            archive_path: zip_dest.to_string_lossy().into_owned(),
            member_path: MemberPath::parse("").unwrap(),
        },
        request_token: 1,
        password: None,
    };
    let list_res_zip = zip_prov.list(&list_req_zip).unwrap();
    assert_eq!(list_res_zip.entries.len(), 1);
    let entry_zip = &list_res_zip.entries[0];
    assert!(
        entry_zip.modified_ms.is_some(),
        "ZIP entry must carry mtime"
    );
    let zip_mtime_ms = entry_zip.modified_ms.unwrap();
    let diff_zip = (zip_mtime_ms - disk_ms).abs();
    assert!(
        diff_zip <= 2000,
        "ZIP mtime difference ({diff_zip} ms) must be within 2000 ms tolerance"
    );

    // 2. 7z mtime (FER-66)
    let sevenz_dest = tmp.path().join("mtime.7z");
    let sevenz_prov = SevenZProvider::new();
    sevenz_prov
        .create(
            &create_req(
                ArchiveCreateFormat::Sevenz,
                vec![src.to_string_lossy().into_owned()],
                &sevenz_dest,
                Some(ArchiveCompressionLevel::Normal),
            ),
            &mut |_| true,
        )
        .unwrap();

    let list_req_7z = ArchiveListRequest {
        location: Location::Archive {
            archive_path: sevenz_dest.to_string_lossy().into_owned(),
            member_path: MemberPath::parse("").unwrap(),
        },
        request_token: 2,
        password: None,
    };
    let list_res_7z = sevenz_prov.list(&list_req_7z).unwrap();
    assert_eq!(list_res_7z.entries.len(), 1);
    let entry_7z = &list_res_7z.entries[0];
    assert!(entry_7z.modified_ms.is_some(), "7z entry must carry mtime");
    let sevenz_mtime_ms = entry_7z.modified_ms.unwrap();
    let diff_7z = (sevenz_mtime_ms - disk_ms).abs();
    assert!(
        diff_7z <= 2000,
        "7z mtime difference ({diff_7z} ms) must be within 2000 ms"
    );
}

#[test]
fn test_request_without_level_defaults_to_normal() {
    let tmp = TempDir::new("no-level");
    let src = tmp.path().join("default.txt");
    fs::write(&src, b"Default compression level test content.").unwrap();

    let zip_dest = tmp.path().join("default.zip");
    let zip_req = create_req(
        ArchiveCreateFormat::Zip,
        vec![src.to_string_lossy().into_owned()],
        &zip_dest,
        None, // None => default Normal
    );
    let zip_prov = ZipProvider::new();
    let res = zip_prov.create(&zip_req, &mut |_| true);
    assert!(res.is_ok(), "create zip without level failed: {:?}", res);

    let extract_dir = tmp.path().join("extract-default");
    fs::create_dir_all(&extract_dir).unwrap();
    zip_prov
        .extract(
            &ArchiveExtractRequest {
                archive_path: zip_dest.to_string_lossy().into_owned(),
                member_paths: vec![],
                destination_path: extract_dir.to_string_lossy().into_owned(),
                conflict: ArchiveConflictPolicy::Overwrite,
                password: None,
                limits: limits(),
            },
            &mut |_| true,
        )
        .unwrap();
    assert_eq!(
        fs::read(extract_dir.join("default.txt")).unwrap(),
        b"Default compression level test content."
    );
}
