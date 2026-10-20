// FER-576 (review R1, B1): every condition under which the shell would delete for good is a refusal, and the
// delete verifies that each item really landed inside the Recycle Bin. Windows and the registry are faked.
use super::*;
use std::cell::RefCell;
use std::collections::HashMap;

const MB: u64 = 1024 * 1024;

#[derive(Default)]
struct FakeEnv {
    volumes: HashMap<String, Result<VolumeFacts, &'static str>>,
    links: HashMap<String, Option<bool>>,
    census: HashMap<String, Result<Census, &'static str>>,
    bins: HashMap<String, BinState>,
    capacity_mb: HashMap<String, u64>,
}

impl RecycleEnv for FakeEnv {
    fn is_link(&self, path: &str) -> Option<bool> {
        self.links.get(path).cloned().unwrap_or(Some(false))
    }
    fn volume_of(&self, path: &str) -> Result<VolumeFacts, &'static str> {
        self.volumes
            .get(path)
            .cloned()
            .unwrap_or_else(|| Ok(fixed("D:\\", Some(1000 * MB))))
    }
    fn bin_state(&self, root: &str) -> BinState {
        self.bins.get(root).copied().unwrap_or(BinState::Usable)
    }
    fn bin_capacity_mb(&self, root: &str) -> Option<u64> {
        self.capacity_mb.get(root).copied()
    }
    fn census(&self, path: &str) -> Result<Census, &'static str> {
        self.census.get(path).cloned().unwrap_or(Ok(Census {
            bytes: MB,
            files: 1,
            ..Census::default()
        }))
    }
}

fn fixed(root: &str, total: Option<u64>) -> VolumeFacts {
    VolumeFacts {
        root: root.to_owned(),
        drive_type: DRIVE_FIXED,
        total_bytes: total,
    }
}

fn big(bytes: u64) -> Result<Census, &'static str> {
    Ok(Census {
        bytes,
        files: 1,
        ..Census::default()
    })
}

fn one(path: &str, env: &FakeEnv) -> Assessment {
    assess(&[path.to_owned()], env).remove(0)
}

const ITEM: &str = r"D:\Work\a.csv";

#[test]
fn a_small_item_on_a_usable_fixed_volume_is_accepted_with_its_real_size() {
    let mut env = FakeEnv::default();
    env.capacity_mb.insert("D:\\".into(), 100);
    env.census.insert(ITEM.into(), big(10 * MB));
    let verdict = one(ITEM, &env);
    assert!(verdict.ok, "{verdict:?}");
    assert_eq!(verdict.census.bytes, 10 * MB);
}

#[test]
fn lexical_refusals_come_before_anything_else() {
    let env = FakeEnv::default();
    for path in [
        r"\\server\share\a",
        r"\\?\UNC\server\share\a",
        "a.txt",
        r"C:a.txt",
        "",
    ] {
        assert_eq!(
            one(path, &env).reason,
            Some(reason::NETWORK_OR_UNC),
            "{path}"
        );
    }
    for path in [r"C:\x\nul", r"C:\x\CON.txt", r"C:\COM1\a"] {
        assert_eq!(
            one(path, &env).reason,
            Some(reason::RESERVED_NAME),
            "{path}"
        );
    }
}

#[test]
fn only_a_fixed_volume_has_a_recycle_bin() {
    for kind in [0u32, 1, 2, 4, 5, 6] {
        let mut env = FakeEnv::default();
        env.volumes.insert(
            ITEM.into(),
            Ok(VolumeFacts {
                root: "E:\\".into(),
                drive_type: kind,
                total_bytes: Some(MB),
            }),
        );
        assert_eq!(
            one(ITEM, &env).reason,
            Some(reason::NO_RECYCLE_BIN),
            "{kind}"
        );
    }
}

#[test]
fn a_volume_set_to_delete_immediately_or_with_a_zero_bin_is_refused() {
    let mut env = FakeEnv::default();
    env.bins.insert("D:\\".into(), BinState::Disabled);
    assert_eq!(one(ITEM, &env).reason, Some(reason::RECYCLE_DISABLED));
}

#[test]
fn an_unreadable_bin_setting_is_refused_fail_closed() {
    let mut env = FakeEnv::default();
    env.bins.insert("D:\\".into(), BinState::Unknown);
    assert_eq!(one(ITEM, &env).reason, Some(reason::RECYCLE_UNKNOWN));
}

#[test]
fn a_mounted_or_redirected_or_unknown_volume_is_refused() {
    for code in [
        reason::MOUNT_POINT,
        reason::REDIRECTED,
        reason::VOLUME_UNKNOWN,
    ] {
        let mut env = FakeEnv::default();
        env.volumes.insert(ITEM.into(), Err(code));
        assert_eq!(one(ITEM, &env).reason, Some(code));
    }
}

#[test]
fn the_volume_decides_not_the_drive_letter() {
    // The letter says D: but the item really sits on a removable volume.
    let mut env = FakeEnv::default();
    env.volumes.insert(
        ITEM.into(),
        Ok(VolumeFacts {
            root: "D:\\".into(),
            drive_type: 2,
            total_bytes: Some(MB),
        }),
    );
    assert_eq!(one(ITEM, &env).reason, Some(reason::NO_RECYCLE_BIN));
}

#[test]
fn a_link_item_is_refused_and_an_uninspectable_one_too() {
    let mut env = FakeEnv::default();
    env.links.insert(ITEM.into(), Some(true));
    assert_eq!(one(ITEM, &env).reason, Some(reason::LINK_ITEM));
    let mut env = FakeEnv::default();
    env.links.insert(ITEM.into(), None);
    assert_eq!(one(ITEM, &env).reason, Some(reason::SIZE_UNKNOWN));
}

#[test]
fn a_size_that_cannot_be_measured_is_refused() {
    let mut env = FakeEnv::default();
    env.census.insert(ITEM.into(), Err(reason::SIZE_UNKNOWN));
    assert_eq!(one(ITEM, &env).reason, Some(reason::SIZE_UNKNOWN));
}

#[test]
fn an_item_larger_than_half_the_bin_capacity_is_refused_as_too_big() {
    let mut env = FakeEnv::default();
    env.capacity_mb.insert("D:\\".into(), 100);
    env.census.insert(ITEM.into(), big(51 * MB));
    assert_eq!(one(ITEM, &env).reason, Some(reason::TOO_BIG));
    env.census.insert(ITEM.into(), big(50 * MB));
    assert!(one(ITEM, &env).ok);
}

#[test]
fn the_quota_is_shared_by_the_whole_batch_on_a_volume() {
    let mut env = FakeEnv::default();
    env.capacity_mb.insert("D:\\".into(), 100);
    let (a, b) = (r"D:\Work\a.bin", r"D:\Work\b.bin");
    env.census.insert(a.into(), big(30 * MB));
    env.census.insert(b.into(), big(30 * MB));
    let verdicts = assess(&[a.to_owned(), b.to_owned()], &env);
    assert!(
        verdicts
            .iter()
            .all(|v| !v.ok && v.reason == Some(reason::TOO_BIG))
    );
    assert!(first_refusal(&verdicts).is_some());
    assert!(assess(&[a.to_owned()], &env)[0].ok);
}

#[test]
fn another_volume_has_its_own_quota() {
    let mut env = FakeEnv::default();
    env.capacity_mb.insert("D:\\".into(), 100);
    env.capacity_mb.insert("E:\\".into(), 1000);
    let (a, b) = (r"D:\Work\a.bin", r"E:\Work\b.bin");
    env.volumes
        .insert(b.into(), Ok(fixed("E:\\", Some(10_000 * MB))));
    env.census.insert(a.into(), big(60 * MB));
    env.census.insert(b.into(), big(60 * MB));
    let verdicts = assess(&[a.to_owned(), b.to_owned()], &env);
    assert_eq!(verdicts[0].reason, Some(reason::TOO_BIG));
    assert!(verdicts[1].ok);
}

#[test]
fn without_an_explicit_limit_the_default_is_five_percent_of_the_volume() {
    let mut env = FakeEnv::default();
    // 1000 MB volume -> 50 MB default -> at most 25 MB per batch.
    env.census.insert(ITEM.into(), big(25 * MB));
    assert!(one(ITEM, &env).ok);
    env.census.insert(ITEM.into(), big(26 * MB));
    assert_eq!(one(ITEM, &env).reason, Some(reason::TOO_BIG));
}

#[test]
fn without_a_limit_and_without_a_volume_size_it_is_refused() {
    let mut env = FakeEnv::default();
    env.volumes.insert(ITEM.into(), Ok(fixed("D:\\", None)));
    assert_eq!(one(ITEM, &env).reason, Some(reason::CAPACITY_UNKNOWN));
}

#[test]
fn one_bad_item_marks_only_itself_but_the_batch_is_refused_by_the_caller() {
    let mut env = FakeEnv::default();
    env.capacity_mb.insert("D:\\".into(), 1000);
    let (good, bad) = (r"D:\Work\good.txt", r"D:\Work\bad.txt");
    env.volumes.insert(bad.into(), Err(reason::MOUNT_POINT));
    let verdicts = assess(&[good.to_owned(), bad.to_owned()], &env);
    assert!(verdicts[0].ok);
    assert_eq!(first_refusal(&verdicts).map(|v| v.path.as_str()), Some(bad));
}

// ── the verified delete ──

#[derive(Clone)]
enum Landing {
    InBin,
    Same,
    Gone,
    Pending,
    Elsewhere,
}

struct FakeProbe(Landing, String);
impl ItemProbe for FakeProbe {
    fn delete_pending(&self) -> bool {
        matches!(self.0, Landing::Pending)
    }
    fn final_path(&self) -> Option<String> {
        match self.0 {
            Landing::InBin => Some(r"\\?\D:\$Recycle.Bin\S-1-5-21\$R1234.csv".into()),
            Landing::Same => Some(format!(r"\\?\{}", self.1)),
            Landing::Elsewhere => Some(r"\\?\D:\Other\a.csv".into()),
            Landing::Gone | Landing::Pending => None,
        }
    }
}

#[derive(Default)]
struct FakeIo {
    landing: HashMap<String, Landing>,
    trash_error: HashMap<String, String>,
    open_error: HashMap<String, String>,
    unusable_from: RefCell<Option<usize>>,
    calls: RefCell<Vec<String>>,
}

impl RecycleIo for FakeIo {
    fn open_probe(&self, path: &str) -> Result<Box<dyn ItemProbe>, String> {
        self.calls.borrow_mut().push(format!("open:{path}"));
        if let Some(message) = self.open_error.get(path) {
            return Err(message.clone());
        }
        let landing = self.landing.get(path).cloned().unwrap_or(Landing::InBin);
        Ok(Box::new(FakeProbe(landing, path.to_owned())))
    }
    fn trash(&self, path: &str) -> Result<(), String> {
        self.calls.borrow_mut().push(format!("trash:{path}"));
        match self.trash_error.get(path) {
            Some(message) => Err(message.clone()),
            None => Ok(()),
        }
    }
    fn still_usable(&self, path: &str) -> bool {
        let usable = match *self.unusable_from.borrow() {
            Some(from) => {
                self.calls
                    .borrow()
                    .iter()
                    .filter(|c| c.starts_with("trash:"))
                    .count()
                    < from
            }
            None => true,
        };
        self.calls
            .borrow_mut()
            .push(format!("usable:{path}:{usable}"));
        usable
    }
}

fn paths(names: &[&str]) -> Vec<String> {
    names.iter().map(|n| format!(r"D:\Work\{n}")).collect()
}

#[test]
fn every_item_that_lands_in_the_bin_is_counted_once_verified() {
    let io = FakeIo::default();
    let report = delete_verified(&paths(&["a", "b"]), &io);
    assert_eq!((report.requested, report.deleted), (2, 2));
    assert!(report.failed.is_empty() && report.bypassed.is_empty() && report.skipped == 0);
}

#[test]
fn an_item_that_vanished_without_landing_in_the_bin_is_a_permanent_delete_and_stops_the_batch() {
    for landing in [Landing::Gone, Landing::Pending, Landing::Elsewhere] {
        let mut io = FakeIo::default();
        io.landing.insert(r"D:\Work\b".into(), landing);
        let report = delete_verified(&paths(&["a", "b", "c", "d"]), &io);
        assert_eq!(report.deleted, 1);
        assert_eq!(report.bypassed, vec![r"D:\Work\b".to_owned()]);
        assert_eq!(report.skipped, 2);
        // The items after it never reached the shell.
        let calls = io.calls.borrow();
        assert!(!calls.iter().any(|c| c == r"trash:D:\Work\c"));
        assert!(!calls.iter().any(|c| c == r"trash:D:\Work\d"));
    }
}

#[test]
fn an_item_the_shell_left_in_place_is_a_failure_not_a_success_and_not_a_bypass() {
    let mut io = FakeIo::default();
    io.landing.insert(r"D:\Work\a".into(), Landing::Same);
    let report = delete_verified(&paths(&["a", "b"]), &io);
    assert_eq!(report.deleted, 1);
    assert_eq!(report.failed.len(), 1);
    assert!(report.bypassed.is_empty());
}

#[test]
fn a_shell_error_or_an_unopenable_item_is_a_failure_and_the_batch_goes_on() {
    let mut io = FakeIo::default();
    io.trash_error.insert(r"D:\Work\a".into(), "locked".into());
    io.landing.insert(r"D:\Work\a".into(), Landing::Same);
    io.open_error
        .insert(r"D:\Work\b".into(), "cannot open".into());
    let report = delete_verified(&paths(&["a", "b", "c"]), &io);
    assert_eq!(report.deleted, 1);
    assert_eq!(report.failed.len(), 2);
    // The unopenable one never reached the shell.
    assert!(!io.calls.borrow().iter().any(|c| c == r"trash:D:\Work\b"));
}

#[test]
fn settings_that_change_between_the_gate_and_the_call_stop_before_the_shell_is_called() {
    let io = FakeIo::default();
    *io.unusable_from.borrow_mut() = Some(1); // usable for the first trash only
    let report = delete_verified(&paths(&["a", "b", "c"]), &io);
    assert_eq!(report.deleted, 1);
    assert_eq!(report.skipped, 1);
    assert_eq!(report.failed.len(), 1);
    assert_eq!(
        io.calls
            .borrow()
            .iter()
            .filter(|c| c.starts_with("trash:"))
            .count(),
        1
    );
}

// ── the real disk walk ──

#[test]
fn the_census_counts_bytes_files_and_folders_without_following_links() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tree");
    std::fs::create_dir_all(root.join("sub/deeper")).unwrap();
    std::fs::create_dir_all(root.join("empty")).unwrap();
    std::fs::write(root.join("a.txt"), vec![0u8; 100]).unwrap();
    std::fs::write(root.join("sub/b.txt"), vec![0u8; 50]).unwrap();
    std::fs::write(root.join("sub/deeper/c.txt"), vec![0u8; 25]).unwrap();
    let census = census_tree(&root.to_string_lossy()).unwrap();
    assert_eq!(
        census,
        Census {
            bytes: 175,
            files: 3,
            dirs: 4,
            empty_dirs: 1,
            links: 0
        }
    );
    let single = census_tree(&root.join("a.txt").to_string_lossy()).unwrap();
    assert_eq!(
        single,
        Census {
            bytes: 100,
            files: 1,
            dirs: 0,
            empty_dirs: 0,
            links: 0
        }
    );
    assert_eq!(
        census_tree(r"Z:\definitely\missing\x"),
        Err(reason::SIZE_UNKNOWN)
    );
}

#[cfg(windows)]
#[test]
fn a_directory_symlink_inside_is_counted_as_a_link_and_not_descended() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("big.bin"), vec![0u8; 1000]).unwrap();
    let root = dir.path().join("tree");
    std::fs::create_dir_all(&root).unwrap();
    if std::os::windows::fs::symlink_dir(&target, root.join("link")).is_err() {
        return; // creating a symlink needs a privilege; the junction case is covered by the attribute check
    }
    let census = census_tree(&root.to_string_lossy()).unwrap();
    assert_eq!(census.links, 1);
    assert_eq!(census.bytes, 0);
    assert_eq!(system_is_link(&root.join("link")), Some(true));
    assert_eq!(system_is_link(&root), Some(false));
}

#[cfg(windows)]
fn system_is_link(path: &std::path::Path) -> Option<bool> {
    SystemRecycleEnv.is_link(&path.to_string_lossy())
}

#[cfg(windows)]
#[test]
fn the_real_volume_of_a_temp_item_is_a_fixed_drive_letter_root_or_is_refused_never_guessed() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("x.txt");
    std::fs::write(&file, b"x").unwrap();
    match SystemRecycleEnv.volume_of(&file.to_string_lossy()) {
        Ok(volume) => {
            assert_eq!(volume.root.len(), 3);
            assert!(volume.root.ends_with(":\\"));
        }
        Err(code) => assert!(
            [
                reason::MOUNT_POINT,
                reason::REDIRECTED,
                reason::VOLUME_UNKNOWN,
                reason::NETWORK_OR_UNC
            ]
            .contains(&code)
        ),
    }
    // A UNC spelling of this machine and a relative name never produce a volume.
    assert!(
        SystemRecycleEnv
            .volume_of(r"\\localhost\c$\Windows\x.txt")
            .is_err()
    );
    assert!(SystemRecycleEnv.volume_of("x.txt").is_err());
}

// ── review R2 ──

#[test]
fn items_inside_shell_managed_folders_are_refused() {
    let env = FakeEnv::default();
    for path in [
        r"D:\$Recycle.Bin\S-1-5-21\$R1234.csv",
        r"D:\$RECYCLE.BIN\x",
        r"D:\Work\System Volume Information\x.dat",
        r"D:\$Extend\$Deleted\x",
        r"D:\RECYCLER\x",
        r"D:\Work\$recycle.bin.\x",
    ] {
        assert_eq!(one(path, &env).reason, Some(reason::SYSTEM_AREA), "{path}");
    }
    assert!(in_system_area(r"D:/work/system volume information/a"));
    assert!(!in_system_area(r"D:\Work\Recycle Bin notes\a.txt"));
    assert!(one(r"D:\Work\a.csv", &env).ok);
}

#[test]
fn the_real_place_of_the_parent_decides_too() {
    // The volume resolver reports a parent that is really inside the bin (reached through a junction).
    let mut env = FakeEnv::default();
    env.volumes.insert(ITEM.into(), Err(reason::SYSTEM_AREA));
    assert_eq!(one(ITEM, &env).reason, Some(reason::SYSTEM_AREA));
}

#[test]
fn delete_verified_never_counts_an_item_that_was_already_in_the_bin() {
    let io = FakeIo::default();
    let in_bin = r"D:\$Recycle.Bin\S-1\$R1.csv".to_owned();
    let report = delete_verified(std::slice::from_ref(&in_bin), &io);
    assert_eq!(report.deleted, 0);
    assert_eq!(report.failed.len(), 1);
    assert!(
        io.calls
            .borrow()
            .iter()
            .all(|c| !c.starts_with("trash:") && !c.starts_with("open:"))
    );
}

#[test]
fn a_shell_error_does_not_hide_what_happened_to_the_item() {
    // Error, but the item is in the bin: it is recycled.
    let mut io = FakeIo::default();
    io.trash_error.insert(r"D:\Work\a".into(), "aborted".into());
    let report = delete_verified(&paths(&["a"]), &io);
    assert_eq!(
        (report.deleted, report.failed.len(), report.bypassed.len()),
        (1, 0, 0)
    );

    // Error and the item is gone without being in the bin: a permanent delete, the batch stops.
    for landing in [Landing::Gone, Landing::Pending, Landing::Elsewhere] {
        let mut io = FakeIo::default();
        io.trash_error.insert(
            r"D:\Work\a".into(),
            "answered yes to the nuke dialog".into(),
        );
        io.landing.insert(r"D:\Work\a".into(), landing);
        let report = delete_verified(&paths(&["a", "b", "c"]), &io);
        assert_eq!(report.bypassed, vec![r"D:\Work\a".to_owned()]);
        assert_eq!(report.skipped, 2);
        assert!(!io.calls.borrow().iter().any(|c| c == r"trash:D:\Work\b"));
    }

    // Error and the item is still where it was: a plain failure with the shell's message.
    let mut io = FakeIo::default();
    io.trash_error.insert(r"D:\Work\a".into(), "locked".into());
    io.landing.insert(r"D:\Work\a".into(), Landing::Same);
    let report = delete_verified(&paths(&["a", "b"]), &io);
    assert_eq!(
        (report.deleted, report.failed.len(), report.bypassed.len()),
        (1, 1, 0)
    );
    assert_eq!(report.failed[0].message, "locked");
}

#[test]
fn the_census_gate_refuses_what_a_read_may_not_open() {
    use crate::ipc::census_sources;
    for bad in [
        r"\\.\pipe\x",
        r"\\?\GLOBALROOT\Device\x",
        r"\\server\share\x",
        "relative.txt",
        r"C:\a\b.txt:ads",
        r"D:\$Recycle.Bin\x",
        r"D:\System Volume Information",
        "",
        "C:\\a\u{0}b",
    ] {
        assert!(census_sources(&[bad.to_owned()]).is_err(), "{bad:?}");
    }
    assert_eq!(
        census_sources(&[r"D:\Work\a.csv".to_owned()]).unwrap(),
        vec![r"D:\Work\a.csv".to_owned()]
    );
}

/// The count of entries the census predicts is the count Test Archive reports for a real archive, so
/// "the archive holds everything that will be deleted" can be checked by comparing the two numbers.
#[test]
fn the_census_entry_count_equals_the_entries_of_a_real_7z_and_zip_of_the_same_tree() {
    use crate::archive_contract::{ArchiveConflictPolicy, ArchiveLimits};
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("tree");
    std::fs::create_dir_all(root.join("sub/deeper")).unwrap();
    std::fs::create_dir_all(root.join("empty")).unwrap();
    std::fs::write(root.join("a.txt"), b"alpha").unwrap();
    std::fs::write(root.join("sub/b.txt"), b"beta").unwrap();
    std::fs::write(root.join("sub/deeper/c.txt"), b"gamma").unwrap();
    let census = census_tree(&root.to_string_lossy()).unwrap();
    let sources = vec![root.to_string_lossy().into_owned()];

    let seven = dir.path().join("out.7z");
    crate::archive_create_test::create_7z(&sources, &seven).unwrap();
    let report = crate::archive_create_test::test_7z(&seven, None).unwrap();
    assert_eq!(report.entries.len() as u64, census.archive_members());

    let zip = dir.path().join("out.zip");
    let limits = ArchiveLimits {
        max_entries: 100_000,
        max_total_bytes: u64::MAX,
        max_entry_bytes: 512 * 1024 * 1024,
        max_depth: 32,
    };
    crate::archive_create_test::create_zip_progress(
        &sources,
        &zip,
        &limits,
        ArchiveConflictPolicy::Overwrite,
        &mut |_| true,
    )
    .unwrap();
    let report = crate::archive_create_test::test_zip(&zip, None).unwrap();
    assert_eq!(report.entries.len() as u64, census.archive_members());
}

/// The REAL shell and the REAL Recycle Bin on a temp file owned by this test: the file is verified to be inside
/// `$Recycle.Bin`. Ignored by default because it leaves one entry in the current user's bin; run by hand:
/// `cargo test real_recycle_bin -- --ignored`. (The temp folder may be on a volume the gate refuses; then the
/// assessment is printed and nothing is deleted.)
#[cfg(windows)]
#[test]
#[ignore = "puts one temp file into the real Recycle Bin"]
fn real_recycle_bin_receives_and_verifies_a_small_file() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir
        .path()
        .join(format!("ferrum-fer576-{}.txt", std::process::id()));
    std::fs::write(&file, b"x").unwrap();
    let path = file.to_string_lossy().into_owned();
    let verdicts = assess(std::slice::from_ref(&path), &SystemRecycleEnv);
    if let Some(refusal) = first_refusal(&verdicts) {
        eprintln!("gate refused the temp file: {:?}", refusal.reason);
        return;
    }
    let report = delete_verified(&[path], &SystemRecycleIo);
    assert_eq!(
        (report.deleted, report.bypassed.len(), report.failed.len()),
        (1, 0, 0)
    );
}
