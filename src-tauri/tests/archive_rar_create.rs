//! FER-505 integration tests: creating RAR archives via the user's Rar.exe.
//!
//! The production path reads `Rar.exe` from prefs only; these tests call the core
//! (`create_rar_with_tool`) with a tool validated from the host's WinRAR install, or the prefs
//! wrapper (`create_rar_from_prefs_dir`) with a private prefs folder. No test mutates the
//! process environment, so the tests are safe to run in parallel.
//!
//! Verifies, with a real Rar.exe (skipped cleanly when WinRAR is not installed):
//! - Unconfigured / invalid prefs path returns UnsupportedFormat without archiving.
//! - Member names match ZIP for the same selection (nested, Unicode, empty folders).
//! - Selecting `src\x.txt` never pulls in `src\sub\x.txt` (no recursion by Rar).
//! - Junctions inside a selected folder are not followed (D07, same as ZIP).
//! - A destination inside the selected folder never archives itself or staging files.
//! - `-cfg-` defeats a `rar.ini` with `switches=-df` next to Rar.exe.
//! - Cancellation of a real `-m5` run kills the child and leaves no job folder/tmp/listfile.
//! - Conflict policies (Skip, Overwrite, Rename) behave like ZIP.

use ferrum_lib::archive_contract::{
    ArchiveCompressionLevel, ArchiveConflictPolicy, ArchiveErrorCode, ArchiveExtractRequest,
    ArchiveLimits, ArchiveListRequest, ArchiveProvider, Location, MemberPath,
};
use ferrum_lib::archive_create_test;
use ferrum_lib::archive_rar::RarProvider;
use ferrum_lib::archive_rar_tool::{
    RarTool, create_rar_from_prefs_dir, create_rar_with_tool, fingerprint_exe,
    validate_confirmed_rar_exe,
};
use ferrum_lib::archive_zip::ZipProvider;
use sha2::{Digest, Sha256};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

static SEQ: AtomicU64 = AtomicU64::new(0);

/// Owned temp dir under %TEMP%, deleted on Drop.
struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-rartest-create-{tag}-{}-{}",
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

/// Locate the host's `Rar.exe` (read-only; never sets env):
/// 1. `FERRUM_RAR_EXE` if set (invalid → skip), as in `tests/archive_rar.rs`
/// 2. `C:\Program Files\WinRAR\Rar.exe`
/// 3. `C:\Program Files (x86)\WinRAR\Rar.exe`
fn find_rar_exe() -> Option<PathBuf> {
    if let Some(val) = std::env::var_os("FERRUM_RAR_EXE") {
        let p = PathBuf::from(val);
        return p.is_file().then_some(p);
    }
    [
        r"C:\Program Files\WinRAR\Rar.exe",
        r"C:\Program Files (x86)\WinRAR\Rar.exe",
    ]
    .iter()
    .map(PathBuf::from)
    .find(|p| p.is_file())
}

/// The host's Rar.exe, validated exactly like production does: static checks, then the
/// fingerprint gate with the file's own freshly computed fingerprint (FER-511).
fn rar_tool() -> Option<RarTool> {
    let Some(p) = find_rar_exe() else {
        eprintln!("SKIP: Rar.exe not found on host");
        return None;
    };
    let fp = fingerprint_exe(&p).expect("host Rar.exe fingerprint");
    Some(validate_confirmed_rar_exe(&p, Some(&fp)).expect("host Rar.exe must validate"))
}

/// FER-514: Rar.exe is only run from a folder chain no other account can change. A private copy
/// in %TEMP% therefore only works where %TEMP% itself passes that rule (a clean CI runner or a
/// single-user PC); on a machine whose %TEMP% grants write access to another account the test
/// skips instead of failing. The policy itself is covered by unit tests in `archive_rar_tool`.
fn temp_copy_is_allowed(dir: &Path) -> bool {
    match ferrum_lib::ai::agent_cli::acl::trusted_location_callback_aware(dir) {
        Ok(()) => true,
        Err(why) => {
            eprintln!("SKIP: temp folder is not a trusted Rar.exe location here: {why}");
            false
        }
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

fn sha256_of(path: &Path) -> String {
    let bytes = fs::read(path).unwrap();
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    hex::encode(hasher.finalize())
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

/// All member paths (walking every virtual folder), `/`-separated and sorted.
fn names_with<P: ArchiveProvider>(provider: &P, archive: &Path) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut stack = vec![String::new()];
    while let Some(dir) = stack.pop() {
        let listing = provider
            .list(&list_req(archive, &dir))
            .expect("listing succeeds");
        for e in listing.entries {
            let name = e.member_path.as_str().replace('\\', "/");
            if out.contains(&name) {
                continue;
            }
            if e.is_dir {
                stack.push(name.clone());
            }
            out.push(name);
        }
    }
    out.sort();
    out
}

fn rar_names(archive: &Path) -> Vec<String> {
    names_with(&RarProvider::new(), archive)
}

fn zip_names(archive: &Path) -> Vec<String> {
    names_with(&ZipProvider::new(), archive)
}

fn make_rar(tool: &RarTool, sources: &[String], dest: &Path) -> PathBuf {
    create_rar_with_tool(
        tool,
        sources,
        dest,
        ArchiveConflictPolicy::Overwrite,
        ArchiveCompressionLevel::Normal,
        &mut |_| true,
    )
    .expect("RAR creation should succeed")
}

fn make_zip(sources: &[String], dest: &Path) {
    archive_create_test::create_zip_progress_with_level(
        sources,
        dest,
        &limits(),
        ArchiveConflictPolicy::Overwrite,
        ArchiveCompressionLevel::Normal,
        &mut |_| true,
    )
    .expect("ZIP creation should succeed");
}

/// Names in `dir` that look like Ferrum staging/job leftovers or listfiles.
fn leftovers(dir: &Path) -> Vec<String> {
    fs::read_dir(dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".ferrum-tmp") || n.starts_with("list-"))
        .collect()
}

fn mklink_junction(link: &Path, target: &Path) {
    let ok = std::process::Command::new("cmd")
        .arg("/C")
        .arg("mklink")
        .arg("/J")
        .arg(link)
        .arg(target)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    assert!(ok, "mklink /J must succeed");
}

fn s(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

#[test]
fn test_rar_create_empty_or_unconfigured_path_returns_unsupported() {
    let t = TempDir::new("unconfigured");
    let src = t.path().join("file.txt");
    fs::write(&src, b"hello world").unwrap();
    let dest = t.path().join("output.rar");
    let sources = vec![s(&src)];

    // Empty prefs folder: rar_exe_path unset.
    let config = t.path().join("config-empty");
    fs::create_dir_all(&config).unwrap();
    let err = create_rar_from_prefs_dir(
        &config,
        &sources,
        &dest,
        ArchiveConflictPolicy::Overwrite,
        ArchiveCompressionLevel::Normal,
        &mut |_| true,
    )
    .expect_err("must fail when Rar.exe is not configured");
    assert_eq!(err.code, ArchiveErrorCode::UnsupportedFormat);

    // Configured, but not a Rar.exe.
    let config2 = t.path().join("config-bad");
    fs::create_dir_all(&config2).unwrap();
    let prefs = ferrum_lib::prefs::Prefs {
        rar_exe_path: Some(r"C:\Windows\System32\cmd.exe".into()),
        ..Default::default()
    };
    ferrum_lib::prefs::save_to(&config2, &prefs).unwrap();
    let err = create_rar_from_prefs_dir(
        &config2,
        &sources,
        &dest,
        ArchiveConflictPolicy::Overwrite,
        ArchiveCompressionLevel::Normal,
        &mut |_| true,
    )
    .expect_err("must fail when the configured path is not Rar.exe");
    assert_eq!(err.code, ArchiveErrorCode::UnsupportedFormat);

    assert!(!dest.exists(), "no archive file created");
    assert!(leftovers(t.path()).is_empty());
}

/// FER-511: only a CONFIRMED exe (path + matching fingerprint in prefs) may run. A legacy path
/// without a fingerprint and an exe changed after confirmation are both refused before any
/// process starts — the archive error would otherwise only appear after a real spawn, and no
/// job folder / listfile is ever created.
#[test]
fn test_rar_create_requires_the_confirmed_fingerprint() {
    let Some(host) = find_rar_exe() else {
        eprintln!("SKIP: Rar.exe not found on host");
        return;
    };
    let t = TempDir::new("fp-gate");
    // Private copy: the user's WinRAR install is never touched.
    let tool_dir = t.path().join("tool");
    fs::create_dir_all(&tool_dir).unwrap();
    let private = tool_dir.join("Rar.exe");
    fs::copy(&host, &private).unwrap();
    if !temp_copy_is_allowed(&private) {
        return;
    }

    let src = t.path().join("file.txt");
    fs::write(&src, b"hello").unwrap();
    let sources = vec![s(&src)];
    let store = |fp: Option<ferrum_lib::prefs::RarExeFingerprint>| {
        let config = t.path().join(format!("config-{}", fp.is_some()));
        fs::create_dir_all(&config).unwrap();
        ferrum_lib::prefs::set_rar_exe(&config, Some(s(&private)), fp).unwrap();
        config
    };
    let run = |config: &Path, dest: &Path| {
        create_rar_from_prefs_dir(
            config,
            &sources,
            dest,
            ArchiveConflictPolicy::Overwrite,
            ArchiveCompressionLevel::Normal,
            &mut |_| true,
        )
    };

    // 1. Legacy prefs: path without fingerprint → refused, nothing ran.
    let legacy = store(None);
    let dest1 = t.path().join("out1.rar");
    let err = run(&legacy, &dest1).expect_err("unconfirmed exe must be refused");
    assert_eq!(err.code, ArchiveErrorCode::UnsupportedFormat);
    assert!(
        err.message.contains("belum dikonfirmasi"),
        "message says why: {}",
        err.message
    );
    assert!(!dest1.exists(), "no archive without confirmation");
    assert!(leftovers(t.path()).is_empty(), "no job folder was created");

    // 2. Confirmed fingerprint (set the legitimate way: freshly computed) → create runs.
    let fp = fingerprint_exe(&private).expect("fingerprint of the private copy");
    let confirmed = store(Some(fp));
    let dest2 = t.path().join("out2.rar");
    let written = run(&confirmed, &dest2).expect("confirmed exe runs");
    assert_eq!(written, dest2);
    assert_eq!(rar_names(&dest2), vec!["file.txt".to_string()]);

    // 3. The exe changes AFTER confirmation → refused BEFORE spawn: error is about the
    //    fingerprint, no second archive, no job folder. (A spawn attempt on this modified exe
    //    would still run and produce a DIFFERENT failure — the message proves the gate fired.)
    let mut bytes = fs::read(&private).unwrap();
    bytes.extend_from_slice(b"\0");
    fs::write(&private, &bytes).unwrap();
    let dest3 = t.path().join("out3.rar");
    let err = run(&confirmed, &dest3).expect_err("changed exe must be refused");
    assert_eq!(err.code, ArchiveErrorCode::UnsupportedFormat);
    assert!(
        err.message.contains("berubah sejak dikonfirmasi"),
        "message says why: {}",
        err.message
    );
    assert!(!dest3.exists(), "no archive from a changed exe");
    assert!(leftovers(t.path()).is_empty(), "no job folder was created");
}

#[test]
fn test_rar_create_via_prefs_dir_uses_configured_tool() {
    let Some(p) = find_rar_exe() else {
        eprintln!("SKIP: Rar.exe not found on host");
        return;
    };
    let t = TempDir::new("prefs-ok");
    let src = t.path().join("file.txt");
    fs::write(&src, b"hello").unwrap();
    let config = t.path().join("config");
    fs::create_dir_all(&config).unwrap();
    // FER-511: the fingerprint is set through the legitimate path — freshly computed from the
    // exe itself, exactly what the confirmed set_path flow stores.
    let prefs = ferrum_lib::prefs::Prefs {
        rar_exe_path: Some(s(&p)),
        rar_exe_fingerprint: Some(fingerprint_exe(&p).expect("host fingerprint")),
        ..Default::default()
    };
    ferrum_lib::prefs::save_to(&config, &prefs).unwrap();
    let dest = t.path().join("out.rar");
    let written = create_rar_from_prefs_dir(
        &config,
        &[s(&src)],
        &dest,
        ArchiveConflictPolicy::Overwrite,
        ArchiveCompressionLevel::Fast,
        &mut |_| true,
    )
    .expect("create via prefs");
    assert_eq!(written, dest);
    assert_eq!(rar_names(&dest), vec!["file.txt".to_string()]);
}

#[test]
fn test_rar_create_tree_unicode_empty_dir_matches_zip() {
    let Some(tool) = rar_tool() else { return };

    let t = TempDir::new("tree-unicode");
    let src_dir = t.path().join("source_root");
    fs::create_dir_all(&src_dir).unwrap();

    let file1 = src_dir.join("root.txt");
    fs::write(&file1, b"root content 12345").unwrap();
    let sub = src_dir.join("nested").join("sub");
    fs::create_dir_all(&sub).unwrap();
    let file2 = sub.join("nested_file.txt");
    fs::write(&file2, b"nested content in sub folder").unwrap();
    let u_dir = src_dir.join("folder_üñîçødé");
    fs::create_dir_all(&u_dir).unwrap();
    let file3 = u_dir.join("dokumen-jepang-日本語.txt");
    fs::write(&file3, "こんにちは世界 - Japanese text in UTF-8".as_bytes()).unwrap();
    // Names Rar could misread as switches/listfile references.
    fs::write(src_dir.join("-df.txt"), b"dash").unwrap();
    fs::write(src_dir.join("@list.txt"), b"at").unwrap();
    fs::write(src_dir.join(" lead.txt"), b"space").unwrap();
    // Empty directories (FER-502), incl. a nested empty chain.
    fs::create_dir_all(src_dir.join("empty_folder")).unwrap();
    fs::create_dir_all(src_dir.join("empty_outer").join("empty_inner")).unwrap();

    let sources = vec![s(&src_dir)];
    let rar_dest = t.path().join("archive.rar");
    let zip_dest = t.path().join("archive.zip");

    assert_eq!(make_rar(&tool, &sources, &rar_dest), rar_dest);
    make_zip(&sources, &zip_dest);

    let rn = rar_names(&rar_dest);
    assert_eq!(rn, zip_names(&zip_dest), "RAR members must match ZIP");
    for want in [
        "source_root/empty_folder",
        "source_root/empty_outer/empty_inner",
        "source_root/-df.txt",
        "source_root/@list.txt",
        "source_root/ lead.txt",
    ] {
        assert!(rn.contains(&want.to_string()), "missing {want}: {rn:?}");
    }

    // Extract the RAR and verify contents.
    let extract_dir = t.path().join("extracted_rar");
    fs::create_dir_all(&extract_dir).unwrap();
    let extract_req = ArchiveExtractRequest {
        archive_path: s(&rar_dest),
        destination_path: s(&extract_dir),
        member_paths: Vec::new(),
        conflict: ArchiveConflictPolicy::Overwrite,
        limits: limits(),
        password: None,
    };
    // The application installs the destination guard at startup; extraction refuses without it.
    ferrum_lib::install_extract_guard();
    RarProvider::new()
        .extract(&extract_req, &mut |_| true)
        .expect("RAR extract succeeds");
    let root = extract_dir.join("source_root");
    assert_eq!(sha256_of(&file1), sha256_of(&root.join("root.txt")));
    assert_eq!(
        sha256_of(&file2),
        sha256_of(&root.join("nested").join("sub").join("nested_file.txt"))
    );
    assert_eq!(
        sha256_of(&file3),
        sha256_of(
            &root
                .join("folder_üñîçødé")
                .join("dokumen-jepang-日本語.txt")
        )
    );
    assert!(root.join("empty_folder").is_dir());
    assert!(leftovers(t.path()).is_empty(), "{:?}", leftovers(t.path()));
}

/// P1-a: selecting `src\x.txt` must not pull in `src\sub\x.txt`.
#[test]
fn test_rar_create_same_name_in_subfolder_not_included() {
    let Some(tool) = rar_tool() else { return };
    let t = TempDir::new("twin");
    let src = t.path().join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("x.txt"), b"top").unwrap();
    fs::write(src.join("sub").join("x.txt"), b"nested").unwrap();
    fs::write(src.join("other.txt"), b"other").unwrap();

    let sources = vec![s(&src.join("x.txt"))];
    let rar_dest = t.path().join("twin.rar");
    let zip_dest = t.path().join("twin.zip");
    make_rar(&tool, &sources, &rar_dest);
    make_zip(&sources, &zip_dest);

    assert_eq!(rar_names(&rar_dest), vec!["x.txt".to_string()]);
    assert_eq!(rar_names(&rar_dest), zip_names(&zip_dest));
}

/// P1-b: junctions inside a selected folder are skipped exactly like ZIP (D07).
#[test]
fn test_rar_create_junction_not_followed_matches_zip() {
    let Some(tool) = rar_tool() else { return };
    let t = TempDir::new("junction");
    let outside = t.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("secret.txt"), b"must not be archived").unwrap();

    let dir = t.path().join("dir");
    fs::create_dir_all(dir.join("real")).unwrap();
    fs::write(dir.join("a.txt"), b"a").unwrap();
    fs::write(dir.join("real").join("b.txt"), b"b").unwrap();
    mklink_junction(&dir.join("junc"), &outside);
    // A folder whose only child is a junction: archived as an empty folder.
    fs::create_dir_all(dir.join("only_link")).unwrap();
    mklink_junction(&dir.join("only_link").join("inner_junc"), &outside);

    let sources = vec![s(&dir)];
    let rar_dest = t.path().join("j.rar");
    let zip_dest = t.path().join("j.zip");
    make_rar(&tool, &sources, &rar_dest);
    make_zip(&sources, &zip_dest);

    let rn = rar_names(&rar_dest);
    assert_eq!(rn, zip_names(&zip_dest), "RAR members must match ZIP");
    assert!(
        rn.iter()
            .all(|n| !n.contains("junc") && !n.contains("secret")),
        "junction content must not be archived: {rn:?}"
    );
    assert!(rn.contains(&"dir/a.txt".to_string()));
    assert!(rn.contains(&"dir/real/b.txt".to_string()));
    assert!(rn.contains(&"dir/only_link".to_string()));
}

fn build_self_tree(root: &Path, ext: &str) -> PathBuf {
    let src = root.join("src");
    fs::create_dir_all(src.join("sub")).unwrap();
    fs::write(src.join("a.txt"), b"a").unwrap();
    fs::write(src.join("sub").join("b.txt"), b"b").unwrap();
    // Stale staging files/job folder of other (crashed/concurrent) archive writes.
    fs::write(src.join(".old.zip.123-4.ferrum-tmp"), b"staging").unwrap();
    fs::write(src.join(".old.rar.123-5.ferrum-tmp.rar"), b"staging").unwrap();
    let stale_job = src.join(".job.rar.77-99.ferrum-tmp");
    fs::create_dir_all(&stale_job).unwrap();
    fs::write(stale_job.join("list-0.txt"), b"x").unwrap();
    // The destination already exists inside the selected folder (Overwrite replaces it).
    fs::write(src.join(format!("out.{ext}")), b"previous archive").unwrap();
    src
}

/// Destination inside the selected folder: never archives itself or staging/job files.
#[test]
fn test_rar_create_destination_inside_source_excludes_self_and_staging() {
    let Some(tool) = rar_tool() else { return };
    let t = TempDir::new("self");
    let rar_src = build_self_tree(&t.path().join("A"), "rar");
    let zip_src = build_self_tree(&t.path().join("B"), "zip");

    let rar_dest = rar_src.join("out.rar");
    let zip_dest = zip_src.join("out.zip");
    make_rar(&tool, &[s(&rar_src)], &rar_dest);
    make_zip(&[s(&zip_src)], &zip_dest);

    let rn = rar_names(&rar_dest);
    assert_eq!(rn, zip_names(&zip_dest), "RAR members must match ZIP");
    assert!(rn.contains(&"src/a.txt".to_string()));
    assert!(rn.contains(&"src/sub/b.txt".to_string()));
    assert!(
        rn.iter()
            .all(|n| !n.contains("out.rar") && !n.contains("ferrum-tmp") && !n.contains("list-")),
        "{rn:?}"
    );
    // Only the pre-existing stale staging entries remain; no new job folder is left.
    let left: Vec<String> = leftovers(&rar_src)
        .into_iter()
        .filter(|n| !n.starts_with(".old.") && !n.starts_with(".job.rar.77-99"))
        .collect();
    assert!(left.is_empty(), "job folder left behind: {left:?}");
}

/// `-cfg-` alone defeats `rar.ini` (read from the folder of Rar.exe) with `switches=-df`.
/// Uses a private copy of Rar.exe in a temp folder; the user's WinRAR config is never touched.
#[test]
fn test_rar_create_cfg_switch_ignores_rar_ini_next_to_exe() {
    let Some(host) = find_rar_exe() else {
        eprintln!("SKIP: Rar.exe not found on host");
        return;
    };
    let t = TempDir::new("cfg-ini");
    let tool_dir = t.path().join("tool");
    fs::create_dir_all(&tool_dir).unwrap();
    let private_rar = tool_dir.join("Rar.exe");
    fs::copy(&host, &private_rar).unwrap();
    if !temp_copy_is_allowed(&private_rar) {
        return;
    }
    fs::write(tool_dir.join("rar.ini"), "switches=-df\r\n").unwrap();

    // Control: without -cfg- this rar.ini really deletes the source (the trap is armed).
    let ctl_src = t.path().join("control.txt");
    fs::write(&ctl_src, b"control").unwrap();
    let status = std::process::Command::new(&private_rar)
        .args(["a", "-idq", "-y", "--", "control.rar", "control.txt"])
        .current_dir(t.path())
        .env_remove("RAR")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(
        !ctl_src.exists(),
        "control: rar.ini switches=-df must be honoured without -cfg- (else this test proves nothing)"
    );

    // Ferrum's run with the same Rar.exe: -cfg- keeps the source intact.
    let tool = {
        let fp = fingerprint_exe(&private_rar).expect("private copy fingerprint");
        validate_confirmed_rar_exe(&private_rar, Some(&fp)).expect("private Rar.exe validates")
    };
    let src_file = t.path().join("precious_source.txt");
    let source_data = b"CRITICAL USER DATA MUST NEVER BE DELETED BY WINRAR SWITCHES";
    fs::write(&src_file, source_data).unwrap();
    let dest = t.path().join("secure.rar");
    let written = make_rar(&tool, &[s(&src_file)], &dest);
    assert!(written.exists(), "archive was created");
    assert_eq!(
        fs::read(&src_file).unwrap(),
        source_data,
        "SECURITY: source must survive a rar.ini with switches=-df"
    );
}

/// Deterministic but incompressible (xorshift64*) data.
fn write_random(path: &Path, bytes: usize) {
    let mut f = File::create(path).unwrap();
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15 ^ u64::from(std::process::id());
    let mut buf = vec![0u8; 1024 * 1024];
    let mut left = bytes;
    while left > 0 {
        for chunk in buf.as_chunks_mut::<8>().0 {
            x ^= x >> 12;
            x ^= x << 25;
            x ^= x >> 27;
            *chunk = x.wrapping_mul(0x2545_F491_4F6C_DD1D).to_le_bytes();
        }
        let n = left.min(buf.len());
        f.write_all(&buf[..n]).unwrap();
        left -= n;
    }
}

#[test]
fn test_rar_create_cancellation_kills_child_and_cleans_tmp() {
    let Some(tool) = rar_tool() else { return };

    let t = TempDir::new("cancel");
    let src_dir = t.path().join("big");
    fs::create_dir_all(&src_dir).unwrap();
    write_random(&src_dir.join("random.bin"), 64 * 1024 * 1024);

    let dest = t.path().join("cancelled.rar");
    let sources = vec![s(&src_dir)];
    let parent = t.path().to_path_buf();

    let mut first_event: Option<Instant> = None;
    let mut saw_job_archive = false;
    let mut cancel_reason = "";
    let started = Instant::now();
    let res = create_rar_with_tool(
        &tool,
        &sources,
        &dest,
        ArchiveConflictPolicy::Overwrite,
        ArchiveCompressionLevel::Max,
        &mut |p| {
            let first = *first_event.get_or_insert_with(Instant::now);
            // Rar is really running once its archive exists in the job folder.
            if !saw_job_archive {
                saw_job_archive = fs::read_dir(&parent)
                    .unwrap()
                    .filter_map(|e| e.ok())
                    .filter(|e| e.file_name().to_string_lossy().contains(".ferrum-tmp"))
                    .any(|e| e.path().join("archive.rar").exists());
            }
            if p.processed_bytes > 0 {
                cancel_reason = "progress";
                return false;
            }
            if saw_job_archive && first.elapsed() > Duration::from_millis(300) {
                cancel_reason = "running";
                return false;
            }
            true
        },
    );
    let elapsed = started.elapsed();

    let err = res.expect_err("must report cancellation");
    assert_eq!(err.code, ArchiveErrorCode::Cancelled, "{err:?}");
    assert!(
        !cancel_reason.is_empty(),
        "cancel must happen while Rar runs"
    );
    eprintln!("cancelled on {cancel_reason} after {elapsed:?}");

    assert!(!dest.exists(), "no final archive after cancel");
    // The job folder (archive.rar + listfile) is removed; that only succeeds once the killed
    // child has released its handles.
    let left = leftovers(t.path());
    assert!(left.is_empty(), "leftovers after cancel: {left:?}");
    assert!(src_dir.join("random.bin").exists(), "source untouched");
}

#[test]
fn test_rar_create_conflict_policies() {
    let Some(tool) = rar_tool() else { return };

    let t = TempDir::new("conflicts");
    let src = t.path().join("sample.txt");
    fs::write(&src, b"sample text").unwrap();
    let sources = vec![s(&src)];

    let dest = t.path().join("target.rar");
    let original_bytes = b"existing archive before conflict";
    fs::write(&dest, original_bytes).unwrap();

    let run = |policy| {
        create_rar_with_tool(
            &tool,
            &sources,
            &dest,
            policy,
            ArchiveCompressionLevel::Normal,
            &mut |_| true,
        )
    };

    // 1. Skip: Conflict, existing file intact.
    let err = run(ArchiveConflictPolicy::Skip).expect_err("Skip should fail when file exists");
    assert_eq!(err.code, ArchiveErrorCode::Conflict);
    assert_eq!(fs::read(&dest).unwrap(), original_bytes);

    // 2. Rename: first free numbered name, existing file intact.
    let renamed = run(ArchiveConflictPolicy::Rename).expect("Rename should succeed");
    let expected_numbered = t.path().join("target (2).rar");
    assert_eq!(renamed, expected_numbered);
    assert_eq!(
        rar_names(&expected_numbered),
        vec!["sample.txt".to_string()]
    );
    assert_eq!(fs::read(&dest).unwrap(), original_bytes);

    // 3. Overwrite: replaces target.rar.
    let overwritten = run(ArchiveConflictPolicy::Overwrite).expect("Overwrite should succeed");
    assert_eq!(overwritten, dest);
    assert_ne!(fs::read(&dest).unwrap(), original_bytes);
    assert_eq!(rar_names(&dest), vec!["sample.txt".to_string()]);

    assert!(leftovers(t.path()).is_empty(), "{:?}", leftovers(t.path()));
}

/// Sources from two different folders: names relative to each source's own folder, like ZIP.
#[test]
fn test_rar_create_sources_from_different_folders_match_zip() {
    let Some(tool) = rar_tool() else { return };
    let t = TempDir::new("multi");
    let a = t.path().join("left");
    let b = t.path().join("right").join("deep");
    fs::create_dir_all(a.join("docs")).unwrap();
    fs::create_dir_all(&b).unwrap();
    fs::write(a.join("docs").join("1.txt"), b"1").unwrap();
    fs::write(b.join("2.txt"), b"2").unwrap();

    let sources = vec![s(&a.join("docs")), s(&b.join("2.txt"))];
    let rar_dest = t.path().join("m.rar");
    let zip_dest = t.path().join("m.zip");
    make_rar(&tool, &sources, &rar_dest);
    make_zip(&sources, &zip_dest);
    let rn = rar_names(&rar_dest);
    assert_eq!(rn, zip_names(&zip_dest));
    assert!(rn.contains(&"docs/1.txt".to_string()) && rn.contains(&"2.txt".to_string()));
}

/// FER-510: two sources from different folders producing the same relative member name
/// (`same.txt` vs `Same.txt` — Windows matches names case-insensitively) are refused with
/// `Conflict` + `member_path` BEFORE Rar.exe runs: no job folder, no archive, no process.
#[test]
fn test_rar_create_duplicate_member_names_refused_before_rar_runs() {
    let Some(tool) = rar_tool() else { return };
    let t = TempDir::new("dup");
    let a = t.path().join("left");
    let b = t.path().join("right");
    fs::create_dir_all(&a).unwrap();
    fs::create_dir_all(&b).unwrap();
    fs::write(a.join("same.txt"), b"one").unwrap();
    fs::write(b.join("Same.txt"), b"two").unwrap();

    let dest = t.path().join("dup.rar");
    let err = create_rar_with_tool(
        &tool,
        &[s(&a.join("same.txt")), s(&b.join("Same.txt"))],
        &dest,
        ArchiveConflictPolicy::Overwrite,
        ArchiveCompressionLevel::Normal,
        &mut |_| true,
    )
    .expect_err("duplicate member names must be refused");
    assert_eq!(err.code, ArchiveErrorCode::Conflict, "{err:?}");
    assert_eq!(err.member_path.as_deref(), Some("Same.txt"));
    assert!(!dest.exists(), "no archive from duplicate names");
    assert!(
        leftovers(t.path()).is_empty(),
        "no job folder was created — Rar never ran: {:?}",
        leftovers(t.path())
    );
}
