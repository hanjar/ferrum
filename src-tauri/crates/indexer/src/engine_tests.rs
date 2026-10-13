//! Engine tests on synthetic volumes (FER-475): the MFT images come from
//! `ferrum_ntfs_scan::mft::fixture` (the parser's own fixture), the journal
//! is simulated byte-exact (`usn::fixture`).

use super::*;
use crate::usn::fixture as usnfx;
use ferrum_ntfs_scan::mft::fixture::{
    FLAG_DIR, FLAG_IN_USE, MemVolume, data_resident, fref, name_attr, record, std_info_meta, volume,
};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::AtomicBool;

const ROOT: u64 = 5 | (5 << 48);
const SERIAL: u64 = 0xABCD_1234;

fn root_record() -> Vec<u8> {
    record(
        5,
        FLAG_DIR,
        0,
        &[std_info_meta(1, 0x16), name_attr(ROOT, ".", 3)],
    )
}

fn dir(seq: u16, parent: u64, name: &str) -> Vec<u8> {
    record(
        seq,
        FLAG_DIR,
        0,
        &[std_info_meta(2, 0x10), name_attr(parent, name, 1)],
    )
}

fn file(seq: u16, parent: u64, name: &str, size: usize, mtime: u64) -> Vec<u8> {
    record(
        seq,
        FLAG_IN_USE,
        0,
        &[
            std_info_meta(mtime, 0x20),
            name_attr(parent, name, 1),
            data_resident(size),
        ],
    )
}

fn free(seq: u16) -> Vec<u8> {
    record(seq, 0, 0, &[])
}

struct World {
    records: BTreeMap<u64, Vec<u8>>,
    journal_active: bool,
    journal_id: u64,
    first_usn: i64,
    next_usn: i64,
    journal: Vec<(i64, u64, u32)>,
    /// Max records per journal read (exercises multiple rounds).
    per_read: usize,
    saved: HashMap<char, Vec<u8>>,
    removed: usize,
    removed_all: usize,
    logs: Vec<String>,
    open_error: Option<VolumeErrorCode>,
    budget: u64,
    profiles: Vec<String>,
    profiles_error: Option<&'static str>,
    builds: usize,
    volumes: Vec<VolumeId>,
    /// Review L1: what `profiles_changed` reports next (then cleared).
    profiles_flag: bool,
}

impl World {
    fn new() -> World {
        let mut records = BTreeMap::new();
        records.insert(5, root_record());
        // A metafile child that must never surface ($Extend\$UsnJrnl).
        records.insert(11, record(1, FLAG_DIR, 0, &[name_attr(ROOT, "$Extend", 3)]));
        records.insert(30, file(1, fref(11, 1), "$UsnJrnl", 8, 0));
        records.insert(16, dir(1, ROOT, "Users"));
        records.insert(17, dir(1, fref(16, 1), "bob"));
        records.insert(18, file(1, fref(17, 1), "secret.txt", 10, 3));
        records.insert(19, dir(1, fref(16, 1), "alice"));
        records.insert(20, file(1, ROOT, "a.txt", 100, 7));
        records.insert(21, dir(1, ROOT, "Data"));
        records.insert(22, file(1, fref(21, 1), "report.doc", 50, 9));
        World {
            records,
            journal_active: true,
            journal_id: 0x01D0_0000_0000_0042,
            first_usn: 1000,
            next_usn: 1000,
            journal: Vec::new(),
            per_read: 3,
            saved: HashMap::new(),
            removed: 0,
            removed_all: 0,
            logs: Vec::new(),
            open_error: None,
            budget: 64 << 20,
            profiles: vec![r"C:\Users\bob".into(), r"C:\Users\alice".into()],
            profiles_error: None,
            builds: 0,
            profiles_flag: false,
            volumes: vec![VolumeId {
                letter: 'C',
                serial: SERIAL,
            }],
        }
    }

    /// Apply a change to record `rec` and journal it.
    fn change(&mut self, rec: u64, raw: Vec<u8>, reason: u32) {
        let seq = u16::from_le_bytes([raw[0x10], raw[0x11]]);
        self.records.insert(rec, raw);
        self.journal.push((self.next_usn, fref(rec, seq), reason));
        self.next_usn += 100;
    }

    fn image(&self) -> (MemVolume, VolumeGeometry) {
        let n = self.records.keys().max().copied().unwrap_or(16) as usize + 1;
        let mut v: Vec<Option<Vec<u8>>> = vec![None; n.max(64)];
        for (k, r) in &self.records {
            v[*k as usize] = Some(r.clone());
        }
        volume(10, v)
    }
}

struct Fake {
    w: Arc<Mutex<World>>,
    vol: MemVolume,
    g: VolumeGeometry,
}

impl VolumeBackend for Fake {
    fn geometry(&mut self) -> io::Result<VolumeGeometry> {
        self.w.lock().unwrap().builds += 1;
        Ok(self.g)
    }
    fn reader(&mut self) -> &mut dyn VolumeReader {
        &mut self.vol
    }
    fn query_journal(&mut self) -> Result<JournalInfo, JournalError> {
        let w = self.w.lock().unwrap();
        if !w.journal_active {
            return Err(JournalError::NotActive);
        }
        Ok(JournalInfo {
            id: w.journal_id,
            first_usn: w.first_usn,
            next_usn: w.next_usn,
        })
    }
    fn read_journal(&mut self, jid: u64, start: i64) -> Result<Vec<u8>, JournalError> {
        let w = self.w.lock().unwrap();
        if !w.journal_active {
            return Err(JournalError::NotActive);
        }
        if jid != w.journal_id {
            return Err(JournalError::Changed);
        }
        if start < w.first_usn {
            return Err(JournalError::Wrapped);
        }
        let mut recs = Vec::new();
        let mut next = w.next_usn;
        for (usn, frn, reason) in w.journal.iter().filter(|(u, ..)| *u >= start) {
            if recs.len() == w.per_read {
                next = *usn;
                break;
            }
            recs.push(usnfx::v2(*frn, ROOT, *usn, *reason, "n"));
        }
        Ok(usnfx::buffer(next.max(start), &recs))
    }
    fn file_record(&mut self, rec: u64) -> io::Result<Option<Vec<u8>>> {
        Ok(self.w.lock().unwrap().records.get(&rec).cloned())
    }
}

struct FakePlatform(Arc<Mutex<World>>);

impl Platform for FakePlatform {
    fn volumes(&mut self) -> Vec<VolumeId> {
        self.0.lock().unwrap().volumes.clone()
    }
    fn open(&mut self, _: &VolumeId) -> Result<Box<dyn VolumeBackend>, VolumeErrorCode> {
        let w = self.0.lock().unwrap();
        if let Some(e) = w.open_error {
            return Err(e);
        }
        let (vol, g) = w.image();
        Ok(Box::new(Fake {
            w: self.0.clone(),
            vol,
            g,
        }))
    }
    fn profiles(&mut self) -> Result<Vec<String>, &'static str> {
        let w = self.0.lock().unwrap();
        match w.profiles_error {
            Some(e) => Err(e),
            None => Ok(w.profiles.clone()),
        }
    }
    fn load(&mut self, v: &VolumeId) -> Option<Vec<u8>> {
        self.0.lock().unwrap().saved.get(&v.letter).cloned()
    }
    fn save(&mut self, v: &VolumeId, bytes: &[u8]) -> io::Result<()> {
        self.0
            .lock()
            .unwrap()
            .saved
            .insert(v.letter, bytes.to_vec());
        Ok(())
    }
    fn remove(&mut self, v: &VolumeId) -> bool {
        let mut w = self.0.lock().unwrap();
        w.removed += 1;
        w.saved.remove(&v.letter);
        true
    }
    fn remove_all(&mut self) {
        let mut w = self.0.lock().unwrap();
        w.removed_all += 1;
        w.saved.clear();
    }
    fn memory_budget(&self) -> u64 {
        self.0.lock().unwrap().budget
    }
    fn profiles_changed(&mut self) -> bool {
        std::mem::take(&mut self.0.lock().unwrap().profiles_flag)
    }
    fn log(&mut self, msg: &str) {
        self.0.lock().unwrap().logs.push(msg.to_owned());
    }
}

struct NoCtl;
impl Control for NoCtl {
    fn checkpoint(&self) -> bool {
        false
    }
}

fn fake(w: &Arc<Mutex<World>>) -> Fake {
    let (vol, g) = w.lock().unwrap().image();
    Fake {
        w: w.clone(),
        vol,
        g,
    }
}

fn names(ix: &VolumeIndex) -> Vec<String> {
    let mut v: Vec<String> = ix
        .entries()
        .map(|e| ix.path_of(&e).unwrap_or_else(|| format!("?{}", e.name)))
        .collect();
    v.sort();
    v
}

fn shared_scope(w: &Arc<Mutex<World>>) -> Scope {
    Scope::for_volume(Mode::Shared, 'C', &w.lock().unwrap().profiles)
}

// ── build ────────────────────────────────────────────────────────────────

#[test]
fn build_high_performance_indexes_everything_but_metafiles() {
    let w = Arc::new(Mutex::new(World::new()));
    let live = build(&mut fake(&w), &Scope::everything(), 64 << 20, &NoCtl).unwrap();
    assert_eq!(
        names(&live.index),
        vec![
            "Data",
            "Data\\report.doc",
            "Users",
            "Users\\alice",
            "Users\\bob",
            "Users\\bob\\secret.txt",
            "a.txt"
        ]
    );
    assert_eq!(live.index.root_frn(), ROOT);
    assert_eq!(live.journal_id, Some(0x01D0_0000_0000_0042));
    assert_eq!(live.next_usn, 1000);
    let a = live.index.record_entries(20)[0];
    assert_eq!(a.meta.size, 100);
    assert_eq!(a.meta.modified, 7);
    assert_eq!(a.meta.attributes, 0x20);
}

#[test]
fn build_shared_mode_keeps_profile_folders_but_not_their_content() {
    let w = Arc::new(Mutex::new(World::new()));
    let scope = shared_scope(&w);
    let live = build(&mut fake(&w), &scope, 64 << 20, &NoCtl).unwrap();
    let n = names(&live.index);
    assert!(n.contains(&"Users\\bob".to_string()));
    assert!(!n.iter().any(|p| p.contains("secret")));
    assert_eq!(live.excluded, [17u64, 19].into_iter().collect());
}

#[test]
fn build_respects_the_memory_budget() {
    let w = Arc::new(Mutex::new(World::new()));
    match build(&mut fake(&w), &Scope::everything(), 64, &NoCtl) {
        Err(BuildError::Failed(VolumeErrorCode::MemoryLimit)) => {}
        Err(e) => panic!("{e:?}"),
        Ok(_) => panic!("expected the memory limit"),
    }
}

#[test]
fn build_without_journal_is_a_snapshot() {
    let w = Arc::new(Mutex::new(World::new()));
    w.lock().unwrap().journal_active = false;
    let mut f = fake(&w);
    let mut live = build(&mut f, &Scope::everything(), 64 << 20, &NoCtl).unwrap();
    assert_eq!(live.journal_id, None);
    assert!(
        live.header(SERIAL).is_none(),
        "nothing to resume: never saved"
    );
    assert_eq!(
        catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap(),
        CatchUp::UpToDate
    );
}

struct StopNow;
impl Control for StopNow {
    fn checkpoint(&self) -> bool {
        true
    }
}

#[test]
fn build_is_abandoned_on_stop() {
    let w = Arc::new(Mutex::new(World::new()));
    assert!(matches!(
        build(&mut fake(&w), &Scope::everything(), 64 << 20, &StopNow),
        Err(BuildError::Abandoned)
    ));
}

// ── live updates ─────────────────────────────────────────────────────────

fn built(w: &Arc<Mutex<World>>, scope: &Scope) -> (Fake, LiveIndex) {
    let mut f = fake(w);
    let live = build(&mut f, scope, 64 << 20, &NoCtl).unwrap();
    (f, live)
}

#[test]
fn create_rename_modify_hardlink_delete() {
    let w = Arc::new(Mutex::new(World::new()));
    let (mut f, mut live) = built(&w, &Scope::everything());
    {
        let mut w = w.lock().unwrap();
        w.change(
            40,
            file(1, fref(21, 1), "new.txt", 5, 11),
            reason::FILE_CREATE,
        );
        w.change(20, file(1, ROOT, "b.txt", 100, 7), reason::RENAME_NEW_NAME);
        w.change(
            22,
            file(1, fref(21, 1), "report.doc", 77, 12),
            reason::DATA_EXTEND,
        );
        let link = record(
            1,
            FLAG_IN_USE,
            0,
            &[
                std_info_meta(11, 0x20),
                name_attr(fref(21, 1), "new.txt", 1),
                name_attr(ROOT, "link-to-new.txt", 1),
                data_resident(5),
            ],
        );
        w.change(40, link, reason::HARD_LINK_CHANGE);
    }
    assert_eq!(
        catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap(),
        CatchUp::UpToDate
    );
    let n = names(&live.index);
    assert!(n.contains(&"Data\\new.txt".to_string()));
    assert!(n.contains(&"link-to-new.txt".to_string()));
    assert!(n.contains(&"b.txt".to_string()));
    assert!(!n.contains(&"a.txt".to_string()));
    let r = live.index.record_entries(22)[0];
    assert_eq!((r.meta.size, r.meta.modified), (77, 12));
    assert_eq!(live.next_usn, w.lock().unwrap().next_usn);
    assert!(live.dirty);

    // Delete (record freed), and the record reused with a new sequence.
    {
        let mut w = w.lock().unwrap();
        w.change(20, free(2), reason::FILE_DELETE);
        w.change(22, file(3, ROOT, "reused.bin", 1, 13), reason::FILE_CREATE);
    }
    catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap();
    let n = names(&live.index);
    assert!(!n.contains(&"b.txt".to_string()));
    assert!(!n.contains(&"Data\\report.doc".to_string()));
    assert!(n.contains(&"reused.bin".to_string()));
    assert_eq!(live.index.record_frn(22), Some(fref(22, 3)));
}

#[test]
fn replay_is_idempotent() {
    let w = Arc::new(Mutex::new(World::new()));
    let (mut f, mut live) = built(&w, &Scope::everything());
    {
        let mut w = w.lock().unwrap();
        w.change(40, file(1, ROOT, "x.txt", 5, 1), reason::FILE_CREATE);
        w.change(40, file(1, ROOT, "y.txt", 5, 1), reason::RENAME_NEW_NAME);
    }
    catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap();
    let once = names(&live.index);
    // Replay everything again from the start of the journal.
    live.next_usn = 1000;
    catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap();
    assert_eq!(names(&live.index), once);
    assert!(once.contains(&"y.txt".to_string()) && !once.contains(&"x.txt".to_string()));
}

#[test]
fn shared_mode_never_indexes_profile_content_live() {
    let w = Arc::new(Mutex::new(World::new()));
    let scope = shared_scope(&w);
    let (mut f, mut live) = built(&w, &scope);
    {
        let mut w = w.lock().unwrap();
        // New file inside bob's profile: ignored.
        w.change(
            41,
            file(1, fref(17, 1), "diary.txt", 5, 1),
            reason::FILE_CREATE,
        );
        // A public folder moved INTO bob's profile: it and its content go.
        w.change(21, dir(1, fref(17, 1), "Data"), reason::RENAME_NEW_NAME);
    }
    assert_eq!(
        catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap(),
        CatchUp::UpToDate
    );
    let n = names(&live.index);
    assert!(
        !n.iter()
            .any(|p| p.contains("diary") || p.contains("report")),
        "{n:?}"
    );
    assert!(!n.iter().any(|p| p.starts_with("Data")), "{n:?}");
}

#[test]
fn folder_leaving_a_profile_forces_a_rebuild() {
    let w = Arc::new(Mutex::new(World::new()));
    let scope = shared_scope(&w);
    // Bob has a folder with content (never indexed in shared mode).
    {
        let mut ww = w.lock().unwrap();
        ww.records.insert(42, dir(1, fref(17, 1), "Photos"));
        ww.records.insert(43, file(1, fref(42, 1), "p1.jpg", 9, 1));
    }
    let (mut f, mut live) = built(&w, &scope);
    // Bob moves it to the public Data folder.
    w.lock()
        .unwrap()
        .change(42, dir(1, fref(21, 1), "Photos"), reason::RENAME_NEW_NAME);
    assert_eq!(
        catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap(),
        CatchUp::Rebuild(RebuildReason::UnindexedSubtree)
    );
    // A brand-new folder is NOT a reason to rebuild, even when it is
    // renamed in the same batch (Explorer "New folder" + rename), and even
    // when it was created inside the profile and moved out with content in
    // the same batch (all of it is refreshed against the current state).
    let (mut f, mut live) = built(&w, &scope);
    {
        let mut w = w.lock().unwrap();
        w.change(44, dir(1, fref(21, 1), "New folder"), reason::FILE_CREATE);
        w.change(44, dir(1, fref(21, 1), "Fresh"), reason::RENAME_NEW_NAME);
        w.change(47, dir(1, fref(17, 1), "Tmp"), reason::FILE_CREATE);
        w.change(48, file(1, fref(47, 1), "t.txt", 1, 1), reason::FILE_CREATE);
        w.change(47, dir(1, fref(21, 1), "Tmp"), reason::RENAME_NEW_NAME);
    }
    assert_eq!(
        catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap(),
        CatchUp::UpToDate
    );
    let n = names(&live.index);
    assert!(n.contains(&r"Data\Fresh".to_string()), "{n:?}");
    assert!(n.contains(&r"Data\Tmp\t.txt".to_string()), "{n:?}");
}

#[test]
fn recreated_profile_folder_becomes_an_exclusion() {
    let w = Arc::new(Mutex::new(World::new()));
    w.lock().unwrap().records.remove(&19); // alice's folder not there yet
    let scope = shared_scope(&w);
    let (mut f, mut live) = built(&w, &scope);
    {
        let mut w = w.lock().unwrap();
        w.change(45, dir(1, fref(16, 1), "alice"), reason::FILE_CREATE);
        w.change(
            46,
            file(1, fref(45, 1), "mail.pst", 9, 1),
            reason::FILE_CREATE,
        );
    }
    catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap();
    let n = names(&live.index);
    assert!(n.contains(&"Users\\alice".to_string()));
    assert!(!n.iter().any(|p| p.contains("mail.pst")), "{n:?}");
}

#[test]
fn renaming_a_folder_onto_a_profile_path_prunes_its_content() {
    // Review H1: Users\bob.tmp (indexed, with content) renamed to the
    // excluded path Users\carol: content must leave the index at once.
    let w = Arc::new(Mutex::new(World::new()));
    {
        let mut ww = w.lock().unwrap();
        ww.profiles.push(r"C:\Users\carol".into());
        ww.records.insert(50, dir(1, fref(16, 1), "carol.tmp"));
        ww.records
            .insert(51, file(1, fref(50, 1), "inbox.pst", 9, 1));
    }
    let scope = shared_scope(&w);
    let (mut f, mut live) = built(&w, &scope);
    assert!(names(&live.index).contains(&r"Users\carol.tmp\inbox.pst".to_string()));
    w.lock()
        .unwrap()
        .change(50, dir(1, fref(16, 1), "carol"), reason::RENAME_NEW_NAME);
    catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap();
    let n = names(&live.index);
    assert!(n.contains(&r"Users\carol".to_string()), "{n:?}");
    assert!(!n.iter().any(|p| p.contains("inbox")), "{n:?}");
    assert!(live.excluded.contains(&50));
}

#[test]
fn a_reused_profile_record_is_no_longer_excluded() {
    // Review M5: bob's profile folder deleted, its record reused by a
    // public folder: that folder and its content are indexed.
    let w = Arc::new(Mutex::new(World::new()));
    let scope = shared_scope(&w);
    let (mut f, mut live) = built(&w, &scope);
    {
        let mut w = w.lock().unwrap();
        w.change(18, free(2), reason::FILE_DELETE);
        w.change(17, free(2), reason::FILE_DELETE);
        w.change(17, dir(3, fref(21, 1), "Projects"), reason::FILE_CREATE);
        w.change(
            52,
            file(1, fref(17, 3), "plan.txt", 1, 1),
            reason::FILE_CREATE,
        );
    }
    catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap();
    let n = names(&live.index);
    assert!(n.contains(&r"Data\Projects\plan.txt".to_string()), "{n:?}");
    assert!(!live.excluded.contains(&17));
}

struct FlakyRecords {
    inner: Fake,
    fail: Arc<AtomicBool>,
}

impl VolumeBackend for FlakyRecords {
    fn geometry(&mut self) -> io::Result<VolumeGeometry> {
        self.inner.geometry()
    }
    fn reader(&mut self) -> &mut dyn VolumeReader {
        self.inner.reader()
    }
    fn query_journal(&mut self) -> Result<JournalInfo, JournalError> {
        self.inner.query_journal()
    }
    fn read_journal(&mut self, j: u64, s: i64) -> Result<Vec<u8>, JournalError> {
        self.inner.read_journal(j, s)
    }
    fn file_record(&mut self, r: u64) -> io::Result<Option<Vec<u8>>> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(io::Error::from_raw_os_error(5));
        }
        self.inner.file_record(r)
    }
}

#[test]
fn unreadable_records_are_retried_then_rebuilt() {
    // Review M2: the position is not advanced past a batch whose records
    // could not be read; a transient failure loses nothing, a persistent
    // one ends in a rebuild.
    let w = Arc::new(Mutex::new(World::new()));
    let (f, mut live) = built(&w, &Scope::everything());
    let fail = Arc::new(AtomicBool::new(true));
    let mut b = FlakyRecords {
        inner: f,
        fail: fail.clone(),
    };
    w.lock()
        .unwrap()
        .change(40, file(1, ROOT, "late.txt", 1, 1), reason::FILE_CREATE);
    let start = live.next_usn;
    assert_eq!(
        catch_up(&mut b, &mut live, 64 << 20, &NoCtl).unwrap(),
        CatchUp::Interrupted
    );
    assert_eq!(live.next_usn, start, "position kept");
    assert_eq!(live.stats.first_read_error, Some(5));
    fail.store(false, Ordering::SeqCst);
    assert_eq!(
        catch_up(&mut b, &mut live, 64 << 20, &NoCtl).unwrap(),
        CatchUp::UpToDate
    );
    assert!(names(&live.index).contains(&"late.txt".to_string()));
    assert_eq!(live.failed_rounds, 0);
    // Persistent failure.
    fail.store(true, Ordering::SeqCst);
    w.lock()
        .unwrap()
        .change(41, file(1, ROOT, "later.txt", 1, 1), reason::FILE_CREATE);
    let mut last = CatchUp::UpToDate;
    for _ in 0..MAX_FAILED_ROUNDS {
        last = catch_up(&mut b, &mut live, 64 << 20, &NoCtl).unwrap();
    }
    assert_eq!(last, CatchUp::Rebuild(RebuildReason::RecordReadsFailing));
}

#[test]
fn a_journal_appearing_on_a_snapshot_volume_triggers_a_rebuild() {
    let w = Arc::new(Mutex::new(World::new()));
    w.lock().unwrap().journal_active = false;
    let (mut f, mut live) = built(&w, &Scope::everything());
    assert_eq!(
        catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap(),
        CatchUp::UpToDate
    );
    w.lock().unwrap().journal_active = true;
    assert_eq!(
        catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap(),
        CatchUp::Rebuild(RebuildReason::JournalAppeared)
    );
}

#[test]
fn repeated_rebuilds_back_off() {
    let base = Duration::from_secs(300);
    assert_eq!(rebuild_delay(1, base), Duration::ZERO);
    assert_eq!(rebuild_delay(2, base), base);
    assert_eq!(rebuild_delay(3, base), base * 2);
    assert_eq!(rebuild_delay(40, base), REBUILD_BACKOFF_MAX);
}

#[test]
fn shared_build_prunes_profiles_early_under_memory_pressure() {
    // Review M4: a big profile must not make a shared-mode build fail when
    // the final (pruned) index fits the budget.
    let mut w = World::new();
    for i in 0..1500u64 {
        w.records
            .insert(200 + i, file(1, fref(17, 1), &format!("{i:€>200}"), 1, 1));
    }
    let w = Arc::new(Mutex::new(w));
    let scope = shared_scope(&w);
    let hp = build(&mut fake(&w), &Scope::everything(), 64 << 20, &NoCtl).unwrap();
    let full = hp.index.memory_bytes();
    let small = build(&mut fake(&w), &scope, 64 << 20, &NoCtl).unwrap();
    let pruned = small.index.memory_bytes();
    assert!(pruned < full / 4, "{pruned} vs {full}");
    // A budget between the two: shared mode still builds, HP does not.
    let budget = (pruned + full) / 2;
    assert!(build(&mut fake(&w), &scope, budget, &NoCtl).is_ok());
    assert!(matches!(
        build(&mut fake(&w), &Scope::everything(), budget, &NoCtl),
        Err(BuildError::Failed(VolumeErrorCode::MemoryLimit))
    ));
}

#[test]
fn journal_wrap_and_id_change_require_rebuild() {
    let w = Arc::new(Mutex::new(World::new()));
    let (mut f, mut live) = built(&w, &Scope::everything());
    w.lock().unwrap().first_usn = 5000;
    assert_eq!(
        catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap(),
        CatchUp::Rebuild(RebuildReason::JournalWrapped)
    );
    let (mut f, mut live) = built(&w, &Scope::everything());
    w.lock().unwrap().journal_id += 1;
    assert_eq!(
        catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap(),
        CatchUp::Rebuild(RebuildReason::JournalChanged)
    );
}

#[test]
fn corrupt_and_unreadable_records_keep_the_old_entry() {
    let w = Arc::new(Mutex::new(World::new()));
    let (mut f, mut live) = built(&w, &Scope::everything());
    let mut bogus = file(1, ROOT, "a.txt", 1, 1);
    bogus[0x38 + 4..0x38 + 8].copy_from_slice(&5000u32.to_le_bytes());
    w.lock().unwrap().change(20, bogus, reason::DATA_EXTEND);
    // Review L3: a corrupt record is retried like a failed read (position
    // kept), and ends in a rebuild when it stays corrupt.
    let start = live.next_usn;
    assert_eq!(
        catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap(),
        CatchUp::Interrupted
    );
    assert_eq!(live.next_usn, start);
    assert_eq!(live.stats.corrupt, 1);
    assert_eq!(live.index.record_entries(20)[0].meta.size, 100);
    let mut last = CatchUp::Interrupted;
    for _ in 1..MAX_FAILED_ROUNDS {
        last = catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap();
    }
    assert_eq!(last, CatchUp::Rebuild(RebuildReason::RecordReadsFailing));
}

#[test]
fn folder_deleted_from_a_profile_into_the_recycle_bin_stays_out() {
    // Review M1: Users\bob\Photos (never indexed) is deleted to
    // $Recycle.Bin\<SID>\$R1: no rebuild, nothing of it indexed.
    let w = Arc::new(Mutex::new(World::new()));
    {
        let mut ww = w.lock().unwrap();
        ww.records.insert(60, dir(1, ROOT, "$Recycle.Bin"));
        ww.records
            .insert(61, dir(1, fref(60, 1), "S-1-5-21-1-2-3-1001"));
        ww.records.insert(42, dir(1, fref(17, 1), "Photos"));
        ww.records.insert(43, file(1, fref(42, 1), "p1.jpg", 9, 1));
    }
    let scope = shared_scope(&w);
    let (mut f, mut live) = built(&w, &scope);
    let n = names(&live.index);
    assert!(n.contains(&"$Recycle.Bin".to_string()), "{n:?}");
    assert!(!n.iter().any(|p| p.contains("S-1-5")), "{n:?}");
    w.lock()
        .unwrap()
        .change(42, dir(1, fref(61, 1), "$R1"), reason::RENAME_NEW_NAME);
    w.lock().unwrap().change(
        44,
        file(1, fref(61, 1), "$Iabc.txt", 1, 1),
        reason::FILE_CREATE,
    );
    assert_eq!(
        catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap(),
        CatchUp::UpToDate
    );
    let n = names(&live.index);
    assert!(
        !n.iter()
            .any(|p| p.contains("R1") || p.contains("p1.jpg") || p.contains("$I")),
        "{n:?}"
    );
    // A full rebuild in shared mode does not index it either.
    let (_, rebuilt) = built(&w, &scope);
    assert!(!names(&rebuilt.index).iter().any(|p| p.contains("p1.jpg")));
    // high_performance indexes the recycle bin normally.
    let (_, hp) = built(&w, &Scope::everything());
    assert!(names(&hp.index).iter().any(|p| p.contains("p1.jpg")));
}

#[test]
fn worker_fails_closed_without_a_profile_list() {
    // Review M2: shared mode + unreadable profile list -> nothing is
    // opened, built or saved; recovers once the list is readable.
    let w = Arc::new(Mutex::new(World::new()));
    w.lock().unwrap().profiles_error = Some("ProfileList not readable");
    let shared = IndexShared::new(Mode::Shared);
    run_until(&w, &shared, |_, s| {
        s.status()
            .first()
            .is_some_and(|v| v.error == VolumeErrorCode::ProfilesUnavailable)
    });
    {
        let ww = w.lock().unwrap();
        assert_eq!(ww.builds, 0);
        assert!(ww.saved.is_empty());
        assert!(ww.logs.iter().any(|l| l.contains("fail closed")));
        assert!(
            !ww.logs.iter().any(|l| l.contains(r"\")),
            "no paths in logs"
        );
    }
    // high_performance does not need the list.
    let hp = IndexShared::new(Mode::HighPerformance);
    run_until(&w, &hp, ready);
    // Shared again with a readable list: rebuilt in the shared scope.
    w.lock().unwrap().profiles_error = None;
    let shared = IndexShared::new(Mode::Shared);
    run_until(&w, &shared, |w, s| {
        ready(w, s) && w.saved.contains_key(&'C')
    });
}

#[test]
fn a_volume_inside_a_profile_is_never_indexed_in_shared_mode() {
    // Review M3: the platform reports `C:\` (volume mounted in a profile).
    let w = Arc::new(Mutex::new(World::new()));
    w.lock().unwrap().profiles.push(r"C:\".into());
    w.lock().unwrap().saved.insert('C', vec![1, 2, 3]);
    let shared = IndexShared::new(Mode::Shared);
    run_until(&w, &shared, |_, s| {
        s.status()
            .first()
            .is_some_and(|v| v.error == VolumeErrorCode::PrivateVolume)
    });
    let ww = w.lock().unwrap();
    assert_eq!(ww.builds, 0);
    assert!(ww.saved.is_empty(), "stale saved index removed");
}

#[test]
fn junction_relocated_profiles_are_excluded_on_the_target_volume() {
    // Review M3: C:\Users -> D:\Users. The platform resolves the final
    // path; D: gets the exclusion even though ProfileList says C:.
    let p = vec![r"C:\Users\bob".to_string(), r"D:\Users\bob".to_string()];
    let d = Scope::for_volume(Mode::Shared, 'D', &p);
    assert!(d.excluded.iter().any(|e| e == &["Users", "bob"]));
}

#[test]
fn mode_change_is_applied_while_paused() {
    // Review L2.
    let w = Arc::new(Mutex::new(World::new()));
    let shared = IndexShared::new(Mode::HighPerformance);
    let (w2, s2) = (w.clone(), shared.clone());
    let h = std::thread::spawn(move || run(&mut FakePlatform(w2), &s2, fast()));
    let deadline = Instant::now() + Duration::from_secs(20);
    while !(ready(&w.lock().unwrap(), &shared) && w.lock().unwrap().saved.contains_key(&'C')) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    shared.paused.store(true, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(50));
    shared.set_mode(Mode::Shared);
    let deadline = Instant::now() + Duration::from_secs(20);
    while w.lock().unwrap().removed_all == 0 {
        assert!(
            Instant::now() < deadline,
            "old-scope files removed while paused"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(w.lock().unwrap().saved.is_empty());
    assert!(shared.status().is_empty(), "in-memory indexes dropped");
    assert_eq!(w.lock().unwrap().builds, 1, "nothing rebuilt while paused");
    shared.stop.store(true, Ordering::SeqCst);
    h.join().unwrap();
}

#[test]
fn pause_abandons_work_so_the_volume_handle_closes() {
    // Review L7: the checkpoint no longer blocks while paused.
    let shared = IndexShared::new(Mode::HighPerformance);
    let ctl = WorkerCtl {
        shared: &shared,
        epoch: shared.epoch(),
        letter: Cell::new(b'C'),
        pass: 1,
    };
    assert!(!ctl.checkpoint());
    shared.paused.store(true, Ordering::SeqCst);
    assert!(ctl.checkpoint());
    let w = Arc::new(Mutex::new(World::new()));
    assert!(matches!(
        build(&mut fake(&w), &Scope::everything(), 64 << 20, &ctl),
        Err(BuildError::Abandoned)
    ));
}

#[test]
fn memory_limit_during_live_updates() {
    let w = Arc::new(Mutex::new(World::new()));
    let (mut f, mut live) = built(&w, &Scope::everything());
    w.lock()
        .unwrap()
        .change(40, file(1, ROOT, "x.txt", 5, 1), reason::FILE_CREATE);
    assert_eq!(
        catch_up(&mut f, &mut live, 16, &NoCtl),
        Err(VolumeErrorCode::MemoryLimit)
    );
}

#[test]
fn malformed_journal_data_forces_rebuild() {
    struct Bad(Fake);
    impl VolumeBackend for Bad {
        fn geometry(&mut self) -> io::Result<VolumeGeometry> {
            self.0.geometry()
        }
        fn reader(&mut self) -> &mut dyn VolumeReader {
            self.0.reader()
        }
        fn query_journal(&mut self) -> Result<JournalInfo, JournalError> {
            self.0.query_journal()
        }
        fn read_journal(&mut self, _: u64, start: i64) -> Result<Vec<u8>, JournalError> {
            let mut b = usnfx::buffer(start + 10, &[usnfx::v2(40, ROOT, start, 0x100, "x")]);
            b[8] = 0; // record length 0 → would loop
            b[9] = 0;
            Ok(b)
        }
        fn file_record(&mut self, r: u64) -> io::Result<Option<Vec<u8>>> {
            self.0.file_record(r)
        }
    }
    let w = Arc::new(Mutex::new(World::new()));
    let (f, mut live) = built(&w, &Scope::everything());
    assert_eq!(
        catch_up(&mut Bad(f), &mut live, 64 << 20, &NoCtl).unwrap(),
        CatchUp::Rebuild(RebuildReason::BadJournalData)
    );
}

// ── worker: persistence, recovery, pause, mode change ────────────────────

fn fast() -> Timings {
    Timings {
        poll: Duration::from_millis(5),
        discover: Duration::from_secs(3600),
        retry: Duration::from_millis(5),
        save_every: Duration::from_millis(0),
    }
}

/// Run the worker on its own thread until `until(world, shared)` holds.
fn run_until(
    w: &Arc<Mutex<World>>,
    shared: &Arc<IndexShared>,
    until: impl Fn(&World, &IndexShared) -> bool,
) {
    shared.stop.store(false, Ordering::SeqCst);
    let (w2, s2) = (w.clone(), shared.clone());
    let h = std::thread::spawn(move || run(&mut FakePlatform(w2), &s2, fast()));
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if until(&w.lock().unwrap(), shared) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "timeout; logs: {:?}",
            w.lock().unwrap().logs
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    shared.stop.store(true, Ordering::SeqCst);
    h.join().unwrap();
}

fn ready(_: &World, s: &IndexShared) -> bool {
    s.status()
        .first()
        .is_some_and(|v| v.state == VolumeState::Ready)
}

#[test]
fn worker_builds_saves_then_loads_and_replays() {
    let w = Arc::new(Mutex::new(World::new()));
    let shared = IndexShared::new(Mode::HighPerformance);
    run_until(&w, &shared, |w, s| {
        ready(w, s) && w.saved.contains_key(&'C')
    });
    let st = shared.status()[0].clone();
    assert_eq!(st.letter, b'C');
    assert_eq!(st.entries, 7);
    assert!(st.live && st.memory_bytes > 0);
    assert_eq!(shared.memory().1, 64 << 20);
    assert_eq!(w.lock().unwrap().builds, 1);

    // Offline change, then restart: loaded from disk, journal replayed,
    // no second MFT build.
    w.lock()
        .unwrap()
        .change(40, file(1, ROOT, "offline.txt", 5, 1), reason::FILE_CREATE);
    let shared = IndexShared::new(Mode::HighPerformance);
    let target = w.lock().unwrap().next_usn;
    run_until(&w, &shared, |_, s| {
        s.status().first().is_some_and(|v| v.next_usn == target)
    });
    let ww = w.lock().unwrap();
    assert_eq!(ww.builds, 1, "logs: {:?}", ww.logs);
    assert!(ww.logs.iter().any(|l| l.contains("saved index loaded")));
    assert_eq!(shared.status()[0].entries, 8);
}

#[test]
fn worker_rebuilds_on_wrap_id_change_and_corruption() {
    for damage in ["wrap", "id", "corrupt", "scope"] {
        let w = Arc::new(Mutex::new(World::new()));
        let shared = IndexShared::new(Mode::HighPerformance);
        run_until(&w, &shared, |w, s| {
            ready(w, s) && w.saved.contains_key(&'C')
        });
        {
            let mut ww = w.lock().unwrap();
            match damage {
                "wrap" => ww.first_usn = 9_999_999,
                "id" => ww.journal_id ^= 1,
                "corrupt" => {
                    let b = ww.saved.get_mut(&'C').unwrap();
                    let at = b.len() - 3;
                    b[at] ^= 0xFF;
                }
                _ => {}
            }
            if damage == "wrap" {
                ww.next_usn = 10_000_000;
            }
        }
        // "scope": the saved index was made in high_performance; start in
        // shared mode.
        let mode = if damage == "scope" {
            Mode::Shared
        } else {
            Mode::HighPerformance
        };
        let shared = IndexShared::new(mode);
        run_until(&w, &shared, |w, s| ready(w, s) && w.builds >= 2);
        let ww = w.lock().unwrap();
        assert!(
            ww.logs.iter().any(|l| l.contains("saved index not used")),
            "{damage}: {:?}",
            ww.logs
        );
        assert!(ww.removed >= 1, "{damage}: stale file deleted");
    }
}

#[test]
fn worker_reports_open_errors_and_retries() {
    let w = Arc::new(Mutex::new(World::new()));
    w.lock().unwrap().open_error = Some(VolumeErrorCode::AccessDenied);
    let shared = IndexShared::new(Mode::HighPerformance);
    run_until(&w, &shared, |_, s| {
        s.status().first().is_some_and(|v| {
            v.state == VolumeState::Error && v.error == VolumeErrorCode::AccessDenied
        })
    });
    w.lock().unwrap().open_error = None;
    run_until(&w, &shared, ready);
}

#[test]
fn worker_memory_limit_is_an_error_state() {
    let w = Arc::new(Mutex::new(World::new()));
    w.lock().unwrap().budget = 64;
    let shared = IndexShared::new(Mode::HighPerformance);
    run_until(&w, &shared, |_, s| {
        s.status()
            .first()
            .is_some_and(|v| v.error == VolumeErrorCode::MemoryLimit)
    });
    assert!(w.lock().unwrap().saved.is_empty());
}

#[test]
fn paused_worker_does_no_work() {
    let w = Arc::new(Mutex::new(World::new()));
    let shared = IndexShared::new(Mode::HighPerformance);
    shared.paused.store(true, Ordering::SeqCst);
    let (w2, s2) = (w.clone(), shared.clone());
    let h = std::thread::spawn(move || run(&mut FakePlatform(w2), &s2, fast()));
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(w.lock().unwrap().builds, 0, "no MFT read while paused");
    assert!(shared.status().is_empty());
    shared.paused.store(false, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready(&w.lock().unwrap(), &shared) {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(5));
    }
    // Paused again: journal changes are not applied.
    shared.paused.store(true, Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(50));
    let before = shared.status()[0].next_usn;
    w.lock().unwrap().change(
        40,
        file(1, ROOT, "while-paused.txt", 1, 1),
        reason::FILE_CREATE,
    );
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(shared.status()[0].next_usn, before);
    shared.stop.store(true, Ordering::SeqCst);
    h.join().unwrap();
}

#[test]
fn mode_change_drops_everything_and_rebuilds_in_the_new_scope() {
    let w = Arc::new(Mutex::new(World::new()));
    let shared = IndexShared::new(Mode::HighPerformance);
    let (w2, s2) = (w.clone(), shared.clone());
    let h = std::thread::spawn(move || run(&mut FakePlatform(w2), &s2, fast()));
    let wait = |cond: &dyn Fn() -> bool| {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !cond() {
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(5));
        }
    };
    wait(&|| ready(&w.lock().unwrap(), &shared) && shared.status()[0].entries == 7);
    shared.set_mode(Mode::Shared);
    wait(&|| {
        ready(&w.lock().unwrap(), &shared)
            && shared.status()[0].entries == 6
            && w.lock().unwrap().removed_all == 1
    });
    shared.stop.store(true, Ordering::SeqCst);
    h.join().unwrap();
    // The saved file now holds the shared-mode index (no secret.txt).
    let bytes = w.lock().unwrap().saved.get(&'C').cloned().unwrap();
    let (_, ix) = persist::decode(&bytes, 1 << 30).unwrap();
    assert!(!ix.entries().any(|e| e.name == "secret.txt"));
}

#[test]
fn two_volumes_are_indexed_in_letter_order() {
    let w = Arc::new(Mutex::new(World::new()));
    w.lock().unwrap().volumes.push(VolumeId {
        letter: 'D',
        serial: 7,
    });
    let shared = IndexShared::new(Mode::HighPerformance);
    run_until(&w, &shared, |_, s| {
        let st = s.status();
        st.len() == 2 && st.iter().all(|v| v.state == VolumeState::Ready)
    });
    assert_eq!(shared.status()[1].letter, b'D');
}

// ── FER-476: L5 stale index, L1 profile changes, I5 security epoch ────────

fn spawn_worker(
    w: &Arc<Mutex<World>>,
    shared: &Arc<IndexShared>,
    t: Timings,
) -> std::thread::JoinHandle<()> {
    let (w2, s2) = (w.clone(), shared.clone());
    std::thread::spawn(move || run(&mut FakePlatform(w2), &s2, t))
}

fn wait(what: &str, cond: &dyn Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !cond() {
        assert!(Instant::now() < deadline, "timeout: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn previous_index_is_served_while_a_rebuild_is_pending() {
    // Review L5: a second journal-triggered rebuild in a row is delayed
    // (backoff); meanwhile queries keep the previous index (stale).
    let w = Arc::new(Mutex::new(World::new()));
    let shared = IndexShared::new(Mode::HighPerformance);
    let t = Timings {
        retry: Duration::from_secs(60),
        ..fast()
    };
    let h = spawn_worker(&w, &shared, t);
    wait("ready", &|| ready(&w.lock().unwrap(), &shared));
    let served_now = |s: &IndexShared| s.served().0.first().cloned();
    assert!(served_now(&shared).is_some_and(|v| v.index.is_some() && !v.stale));
    for round in 1..=2 {
        {
            let mut ww = w.lock().unwrap();
            ww.first_usn = 1_000_000 * round;
            ww.next_usn = 1_000_000 * round + 10;
        }
        if round == 1 {
            // Immediate rebuild, then ready again.
            wait("rebuilt", &|| {
                w.lock().unwrap().builds >= 2 && ready(&w.lock().unwrap(), &shared)
            });
        }
    }
    // Second wrap within the window: delayed by `retry` (60 s) — the old
    // index stays searchable, flagged stale, while the volume says building.
    wait("stale served", &|| {
        served_now(&shared).is_some_and(|v| v.stale)
    });
    let v = served_now(&shared).unwrap();
    let ix = v.index.unwrap();
    assert_eq!(read_index(&ix).index.len(), 7);
    assert_eq!(shared.status()[0].state, VolumeState::Building);
    assert_eq!(w.lock().unwrap().builds, 2, "rebuild still pending");
    // A mode change drops the stale index from the served list at once.
    shared.set_mode(Mode::Shared);
    assert!(shared.served().0.is_empty());
    shared.stop.store(true, Ordering::SeqCst);
    h.join().unwrap();
}

#[test]
fn a_new_profile_triggers_rediscovery_at_once() {
    // Review L1: the ProfileList notification makes the worker rediscover
    // in the next poll (periodic discovery is 1 h in `fast()`).
    let w = Arc::new(Mutex::new(World::new()));
    let shared = IndexShared::new(Mode::Shared);
    let h = spawn_worker(&w, &shared, fast());
    wait("ready", &|| {
        ready(&w.lock().unwrap(), &shared) && w.lock().unwrap().saved.contains_key(&'C')
    });
    {
        let mut ww = w.lock().unwrap();
        ww.profiles.push(r"C:\Data".into());
        ww.profiles_flag = true;
    }
    wait("rediscovered", &|| {
        w.lock()
            .unwrap()
            .logs
            .iter()
            .any(|l| l.contains("scope changed"))
    });
    wait("rebuilt without Data content", &|| {
        ready(&w.lock().unwrap(), &shared) && shared.status()[0].entries == 5
    });
    shared.stop.store(true, Ordering::SeqCst);
    h.join().unwrap();
}

#[test]
fn save_rereads_the_profile_list_first() {
    // Review L1 (no notification): a profile added since the last discovery
    // blocks the next save and forces a rediscovery.
    let w = Arc::new(Mutex::new(World::new()));
    let shared = IndexShared::new(Mode::Shared);
    let h = spawn_worker(&w, &shared, fast());
    wait("ready", &|| {
        ready(&w.lock().unwrap(), &shared) && w.lock().unwrap().saved.contains_key(&'C')
    });
    {
        let mut ww = w.lock().unwrap();
        ww.profiles.push(r"C:\Data".into());
        ww.change(
            40,
            file(1, fref(21, 1), "new.doc", 5, 1),
            reason::FILE_CREATE,
        );
    }
    wait("save skipped", &|| {
        w.lock()
            .unwrap()
            .logs
            .iter()
            .any(|l| l.contains("save skipped"))
    });
    wait("rediscovered", &|| {
        w.lock()
            .unwrap()
            .logs
            .iter()
            .any(|l| l.contains("scope changed"))
    });
    shared.stop.store(true, Ordering::SeqCst);
    h.join().unwrap();
    // Whatever is saved now was made with the new scope (no Data content).
    if let Some(bytes) = w.lock().unwrap().saved.get(&'C') {
        let (_, ix) = persist::decode(bytes, 1 << 30).unwrap();
        assert!(
            !ix.entries()
                .any(|e| e.name == "report.doc" || e.name == "new.doc")
        );
    }
}

#[test]
fn security_changes_are_counted() {
    // Review I5: a SECURITY_CHANGE record changes nothing in the index but
    // is counted (the worker turns it into a new security epoch).
    let w = Arc::new(Mutex::new(World::new()));
    let (mut f, mut live) = built(&w, &Scope::everything());
    let before = live.index.len();
    let raw = w.lock().unwrap().records[&22].clone();
    w.lock().unwrap().change(22, raw, reason::SECURITY_CHANGE);
    catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap();
    assert_eq!(live.security_changes, 1);
    assert_eq!(live.index.len(), before);
}

#[test]
fn catch_up_rounds_reports_more_work() {
    let w = Arc::new(Mutex::new(World::new()));
    let (mut f, mut live) = built(&w, &Scope::everything());
    for i in 0..10u64 {
        w.lock().unwrap().change(
            40 + i,
            file(1, ROOT, &format!("f{i}.txt"), 1, 1),
            reason::FILE_CREATE,
        );
    }
    // per_read = 3: one round leaves more to read.
    let (r, more) = catch_up_rounds(&mut f, &mut live, 64 << 20, &NoCtl, 1);
    assert_eq!(r, Ok(CatchUp::Interrupted));
    assert!(more);
    let (r, more) = catch_up_rounds(&mut f, &mut live, 64 << 20, &NoCtl, 100);
    assert_eq!(r, Ok(CatchUp::UpToDate));
    assert!(!more);
    assert_eq!(live.index.len(), 17);
}

#[test]
fn served_snapshot_is_consistent_with_mode_changes() {
    // Review M1: a reader must never pair the list of one mode with the
    // epoch/mode of another. The writer serves a list tagged with the
    // mode right after each switch; readers check the tag.
    let shared = IndexShared::new(Mode::HighPerformance);
    let tag = |m: Mode| {
        if m == Mode::HighPerformance {
            b'H'
        } else {
            b'S'
        }
    };
    let stop = Arc::new(AtomicBool::new(false));
    let (s2, stop2) = (shared.clone(), stop.clone());
    let writer = std::thread::spawn(move || {
        let mut m = Mode::HighPerformance;
        for _ in 0..20_000 {
            m = if m == Mode::HighPerformance {
                Mode::Shared
            } else {
                Mode::HighPerformance
            };
            s2.set_mode(m);
            s2.serve_for_test(vec![ServedVolume {
                letter: tag(m),
                index: None,
                stale: false,
            }]);
        }
        stop2.store(true, Ordering::SeqCst);
    });
    let mut checked = 0u64;
    while !stop.load(Ordering::SeqCst) {
        let (list, epoch, mode) = shared.served();
        if let Some(v) = list.first() {
            assert_eq!(v.letter, tag(mode), "list of another mode");
            // The epoch read with it is the one that mode was set with:
            // a later switch would have bumped it and cleared the list.
            assert!(epoch <= shared.epoch());
            checked += 1;
        }
    }
    writer.join().unwrap();
    assert!(checked > 0);
}

// ── FER-477: change log (incremental sync) and freshness barrier ──────────

fn logged(live: &LiveIndex, since: i64) -> Vec<(u64, bool)> {
    let mut v = live.changes.since(since).expect("log complete");
    v.sort();
    v.dedup();
    v
}

#[test]
fn change_log_records_old_and_new_parents_and_moved_directories() {
    let w = Arc::new(Mutex::new(World::new()));
    let (mut f, mut live) = built(&w, &Scope::everything());
    let start = live.next_usn;
    assert_eq!(
        live.changes.floor(),
        start,
        "complete from the build position"
    );
    assert!(
        live.changes.since(start - 1).is_none(),
        "never before the floor"
    );
    // Move Data\report.doc to Users\alice (old and new parent change).
    w.lock().unwrap().change(
        22,
        file(1, fref(19, 1), "report.doc", 50, 9),
        reason::RENAME_NEW_NAME,
    );
    catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap();
    let got = logged(&live, start);
    assert!(
        got.contains(&(fref(21, 1), false)),
        "old parent Data: {got:?}"
    );
    assert!(
        got.contains(&(fref(19, 1), false)),
        "new parent alice: {got:?}"
    );
    assert!(
        !got.iter().any(|(_, r)| *r),
        "a file move is not recursive: {got:?}"
    );
    // A content change logs only the parent.
    let mid = live.next_usn;
    w.lock().unwrap().change(
        22,
        file(1, fref(19, 1), "report.doc", 51, 10),
        reason::DATA_EXTEND,
    );
    catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap();
    let got = logged(&live, mid);
    assert!(got.contains(&(fref(19, 1), false)), "{got:?}");
    assert!(
        !got.contains(&(fref(21, 1), false)),
        "Data untouched since mid"
    );
    // Renaming a directory: its parent changes, and the directory itself is
    // recursive (everything below it is somewhere else now).
    let mid = live.next_usn;
    w.lock()
        .unwrap()
        .change(21, dir(1, ROOT, "Data2"), reason::RENAME_NEW_NAME);
    catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap();
    let got = logged(&live, mid);
    assert!(got.contains(&(ROOT, false)), "{got:?}");
    assert!(got.contains(&(fref(21, 1), true)), "{got:?}");
    // An attribute-only change of a directory is not recursive.
    let mid = live.next_usn;
    w.lock().unwrap().change(
        21,
        record(
            1,
            FLAG_DIR,
            0,
            &[std_info_meta(99, 0x12), name_attr(ROOT, "Data2", 1)],
        ),
        reason::BASIC_INFO_CHANGE,
    );
    catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap();
    let got = logged(&live, mid);
    assert!(got.contains(&(ROOT, false)));
    assert!(!got.contains(&(fref(21, 1), true)), "{got:?}");
    // Deleting a directory record logs its parent and itself (recursive).
    let mid = live.next_usn;
    w.lock().unwrap().change(19, free(2), reason::FILE_DELETE);
    catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap();
    let got = logged(&live, mid);
    assert!(got.contains(&(fref(16, 1), false)), "{got:?}");
    assert!(got.contains(&(fref(19, 1), true)), "{got:?}");
}

#[test]
fn a_retried_batch_still_logs_the_old_parent() {
    // The first attempt moves the record in the index, then the batch is
    // retried (another record failed to read): the retry no longer sees the
    // old parent, so it must have been logged by the first attempt.
    let w = Arc::new(Mutex::new(World::new()));
    let (mut f, mut live) = built(&w, &Scope::everything());
    let start = live.next_usn;
    let mut changes = vec![usn::UsnChange {
        usn: start,
        frn: fref(22, 1),
        parent: 0,
        reason: reason::RENAME_NEW_NAME,
    }];
    w.lock()
        .unwrap()
        .records
        .insert(22, file(1, ROOT, "report.doc", 50, 9));
    apply_changes(&mut f, &mut live, &changes);
    changes[0].usn = start + 1;
    apply_changes(&mut f, &mut live, &changes);
    let got = logged(&live, start);
    assert!(got.contains(&(fref(21, 1), false)), "{got:?}");
    assert!(got.contains(&(ROOT, false)), "{got:?}");
}

#[test]
fn loaded_index_log_starts_at_the_saved_position() {
    let w = Arc::new(Mutex::new(World::new()));
    let (_, live) = built(&w, &Scope::everything());
    let h = live.header(SERIAL).unwrap();
    let loaded = LiveIndex::from_saved(live.index, Scope::everything(), &h);
    assert_eq!(loaded.changes.floor(), h.next_usn);
    assert!(loaded.changes.is_empty());
    assert_eq!(loaded.caught_pass, 0, "not current until a pass caught up");
}

#[test]
fn worker_marks_passes_that_read_the_journal_to_the_end() {
    let w = Arc::new(Mutex::new(World::new()));
    let shared = IndexShared::new(Mode::HighPerformance);
    let t = Timings {
        poll: Duration::from_secs(30),
        ..fast()
    };
    let h = spawn_worker(&w, &shared, t);
    wait("ready", &|| ready(&w.lock().unwrap(), &shared));
    // The poll sleep is 30 s: only a kick makes the next pass happen now.
    let g0 = shared.poll_gen();
    w.lock().unwrap().change(
        40,
        file(1, fref(21, 1), "fresh.txt", 5, 11),
        reason::FILE_CREATE,
    );
    shared.request_poll();
    let ix = shared.served().0[0].index.clone().unwrap();
    wait("kicked pass", &|| read_index(&ix).caught_pass > g0);
    let g = read_index(&ix);
    assert!(names(&g.index).contains(&r"Data\fresh.txt".to_string()));
    drop(g);
    shared.stop.store(true, Ordering::SeqCst);
    h.join().unwrap();
}

// ── FER-477 review P2-3: open write sessions ─────────────────────────────

#[test]
fn sessions_open_before_the_index_are_found_by_the_priming_scan() {
    let w = Arc::new(Mutex::new(World::new()));
    {
        let mut ww = w.lock().unwrap();
        // 22: written, still open. 20: written and closed.
        ww.change(
            22,
            file(1, fref(21, 1), "report.doc", 50, 9),
            reason::DATA_OVERWRITE,
        );
        ww.change(20, file(1, ROOT, "a.txt", 100, 7), reason::DATA_EXTEND);
        ww.change(
            20,
            file(1, ROOT, "a.txt", 100, 7),
            reason::DATA_EXTEND | reason::CLOSE,
        );
    }
    let mut f = fake(&w);
    let mut live = build(&mut f, &Scope::everything(), 64 << 20, &NoCtl).unwrap();
    assert_eq!(live.changes.open_writes().count(), 0, "not primed yet");
    prime_open_writes(&mut f, &mut live, &NoCtl);
    let open: Vec<u64> = live.changes.open_writes().map(|(r, _)| r).collect();
    assert_eq!(open, vec![22]);
    assert!(!live.changes.open_unknown());
    // Its CLOSE arrives through the live journal.
    w.lock().unwrap().change(
        22,
        file(1, fref(21, 1), "report.doc", 60, 10),
        reason::DATA_OVERWRITE | reason::CLOSE,
    );
    catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap();
    assert_eq!(live.changes.open_writes().count(), 0);
    // A write that opens after the index is tracked live too.
    w.lock()
        .unwrap()
        .change(20, file(1, ROOT, "a.txt", 101, 8), reason::DATA_EXTEND);
    catch_up(&mut f, &mut live, 64 << 20, &NoCtl).unwrap();
    let open: Vec<u64> = live.changes.open_writes().map(|(r, _)| r).collect();
    assert_eq!(open, vec![20]);
}

#[test]
fn an_incomplete_priming_scan_makes_the_sessions_unknown() {
    let w = Arc::new(Mutex::new(World::new()));
    let mut f = fake(&w);
    let mut live = build(&mut f, &Scope::everything(), 64 << 20, &NoCtl).unwrap();
    w.lock().unwrap().journal_id ^= 1; // the journal is another one now
    prime_open_writes(&mut f, &mut live, &NoCtl);
    assert!(live.changes.open_unknown());
}

// ── FER-503: recovery from `open_unknown` on a live index ────────────────

use crate::changelog::MAX_OPEN_WRITES;

/// The first priming scan failed (interrupted, e.g. a paused service); the
/// periodic re-prime scans again and, when it completes, recovers the flag
/// AND knows the sessions of the whole window — no interval in which an
/// actually open session is missed without `changes` answering rescan.
#[test]
fn a_completed_reprime_recovers_open_unknown_and_tracks_the_window() {
    let w = Arc::new(Mutex::new(World::new()));
    {
        let mut ww = w.lock().unwrap();
        // 22: written, still open — its record is in the journal window.
        ww.change(
            22,
            file(1, fref(21, 1), "report.doc", 50, 9),
            reason::DATA_OVERWRITE,
        );
    }
    let mut f = fake(&w);
    let mut live = build(&mut f, &Scope::everything(), 64 << 20, &NoCtl).unwrap();
    w.lock().unwrap().journal_id ^= 1; // the first scan cannot run
    prime_open_writes(&mut f, &mut live, &NoCtl);
    assert!(live.changes.open_unknown());

    // The journal is the same again. On the LIVE index (the Arc a step()
    // slot holds) the re-prime runs and recovers.
    w.lock().unwrap().journal_id ^= 1;
    let live = Arc::new(RwLock::new(live));
    assert!(reprime_open_writes(&mut f, &live, &NoCtl), "scan completed");
    let g = read_index(&live);
    assert!(!g.changes.open_unknown(), "recovered");
    // Nothing of the window was missed: the open session is tracked now.
    let open: Vec<u64> = g.changes.open_writes().map(|(r, _)| r).collect();
    assert_eq!(open, vec![22]);
}

/// A failed re-prime (the journal is not the index's, or reads fail)
/// changes nothing: the flag stays and `changes` keeps answering rescan.
#[test]
fn a_failed_reprime_keeps_open_unknown() {
    let w = Arc::new(Mutex::new(World::new()));
    let mut f = fake(&w);
    let live = build(&mut f, &Scope::everything(), 64 << 20, &NoCtl).unwrap();
    let live = Arc::new(RwLock::new(live));
    write_index(&live).changes.set_open_unknown();

    w.lock().unwrap().journal_id ^= 1; // the journal is another one now
    assert!(!reprime_open_writes(&mut f, &live, &NoCtl));
    assert!(read_index(&live).changes.open_unknown());

    w.lock().unwrap().journal_active = false; // reads fail outright
    assert!(!reprime_open_writes(&mut f, &live, &NoCtl));
    assert!(read_index(&live).changes.open_unknown());
}

/// A re-prime over a window that still holds more open sessions than the
/// cap completes, but must NOT clear the flag: the sessions it had to drop
/// stay unknown. The overflow of THIS scan is told apart from a previous
/// one by the counter.
#[test]
fn a_reprime_over_an_overflowing_window_stays_unknown() {
    let w = Arc::new(Mutex::new(World::new()));
    {
        let mut ww = w.lock().unwrap();
        for rec in 100..(100 + MAX_OPEN_WRITES as u64 + 1) {
            ww.change(rec, file(1, ROOT, "x", 1, 1), reason::DATA_OVERWRITE);
        }
    }
    let mut f = fake(&w);
    let mut live = build(&mut f, &Scope::everything(), 64 << 20, &NoCtl).unwrap();
    prime_open_writes(&mut f, &mut live, &NoCtl);
    assert!(live.changes.open_unknown(), "overflows the cap");
    assert_eq!(live.changes.open_overflows(), 1);

    let live = Arc::new(RwLock::new(live));
    assert!(reprime_open_writes(&mut f, &live, &NoCtl), "scan completed");
    let g = read_index(&live);
    assert!(g.changes.open_unknown(), "this scan overflowed too");
    assert_eq!(g.changes.open_overflows(), 2);
    assert_eq!(g.changes.open_writes().count(), MAX_OPEN_WRITES);
}

// ── FER-503 review P2: sessions dropped while following the journal ──────

/// Follow the journal to its end.
fn follow(f: &mut Fake, live: &SharedIndex) {
    loop {
        let r = catch_up(f, &mut write_index(live), 64 << 20, &NoCtl).unwrap();
        if r == CatchUp::UpToDate {
            return;
        }
        assert_eq!(r, CatchUp::Interrupted, "no rebuild expected");
    }
}

/// A primed live index; then MAX_OPEN_WRITES sessions open while it follows
/// the journal, plus one more (the victim, record 9000) that the log has to
/// drop; then every other session closes. Returns the victim's USN.
fn overflow_while_following(w: &Arc<Mutex<World>>) -> (Fake, SharedIndex, i64) {
    w.lock().unwrap().per_read = 512;
    let mut f = fake(w);
    let mut live = build(&mut f, &Scope::everything(), 64 << 20, &NoCtl).unwrap();
    prime_open_writes(&mut f, &mut live, &NoCtl);
    assert!(!live.changes.open_unknown(), "the first scan succeeded");
    let live = Arc::new(RwLock::new(live));
    let victim_usn = {
        let mut ww = w.lock().unwrap();
        for rec in 100..(100 + MAX_OPEN_WRITES as u64) {
            ww.change(rec, file(1, ROOT, "x", 1, 1), reason::DATA_OVERWRITE);
        }
        let usn = ww.next_usn;
        ww.change(9000, file(1, ROOT, "victim", 1, 1), reason::DATA_OVERWRITE);
        usn
    };
    follow(&mut f, &live);
    {
        let g = read_index(&live);
        assert!(g.changes.open_unknown(), "the victim was dropped");
        assert_eq!(g.changes.lowest_dropped_usn(), Some(victim_usn));
        assert!(!g.changes.open_writes().any(|(r, _)| r == 9000));
    }
    {
        let mut ww = w.lock().unwrap();
        for rec in 100..(100 + MAX_OPEN_WRITES as u64) {
            ww.change(
                rec,
                file(1, ROOT, "x", 1, 1),
                reason::DATA_OVERWRITE | reason::CLOSE,
            );
        }
    }
    follow(&mut f, &live);
    assert_eq!(read_index(&live).changes.open_writes().count(), 0);
    (f, live, victim_usn)
}

/// Review P2: the victim's opening record leaves the journal (wrap) before
/// the re-prime. No scan can find it any more, and its handle may keep
/// writing without a new record — the flag must stay (until rebuild or
/// reload), whatever the scan sees.
#[test]
fn a_reprime_after_the_dropped_session_left_the_journal_stays_unknown() {
    let w = Arc::new(Mutex::new(World::new()));
    let (mut f, live, victim_usn) = overflow_while_following(&w);
    w.lock().unwrap().first_usn = victim_usn + 1; // journal wrap
    assert_eq!(
        reprime_begin(&mut f, &live).unwrap_err(),
        ReprimeSkip::OutOfJournal
    );
    assert!(!reprime_open_writes(&mut f, &live, &NoCtl));
    let g = read_index(&live);
    assert!(g.changes.open_unknown(), "the victim is still unknown");
    assert!(!g.changes.open_writes().any(|(r, _)| r == 9000));
}

/// Same overflow, no wrap: the re-prime still sees the victim's record, so
/// it may recover — and the victim is tracked afterwards.
#[test]
fn a_reprime_that_still_sees_the_dropped_session_recovers_and_tracks_it() {
    let w = Arc::new(Mutex::new(World::new()));
    let (mut f, live, victim_usn) = overflow_while_following(&w);
    // Records before the victim may leave the journal: still fine.
    w.lock().unwrap().first_usn = victim_usn;
    assert!(reprime_open_writes(&mut f, &live, &NoCtl), "scan completed");
    let g = read_index(&live);
    assert!(!g.changes.open_unknown(), "recovered");
    assert_eq!(g.changes.lowest_dropped_usn(), None);
    let open: Vec<u64> = g.changes.open_writes().map(|(r, _)| r).collect();
    assert_eq!(open, vec![9000], "the victim is tracked now");
}

struct StopCtl;
impl Control for StopCtl {
    fn checkpoint(&self) -> bool {
        true
    }
}

/// An interrupted FIRST priming scan names the point it stopped at: a
/// session opened after it but before the index position was never seen,
/// so once that point has left the journal the flag stays too; while it is
/// still in the journal a re-prime recovers.
#[test]
fn an_interrupted_priming_scan_is_recovered_only_while_its_window_is_in_the_journal() {
    for wrapped in [false, true] {
        let w = Arc::new(Mutex::new(World::new()));
        let open_usn = {
            let mut ww = w.lock().unwrap();
            let usn = ww.next_usn;
            ww.change(
                22,
                file(1, fref(21, 1), "report.doc", 50, 9),
                reason::DATA_OVERWRITE,
            );
            usn
        };
        let mut f = fake(&w);
        let mut live = build(&mut f, &Scope::everything(), 64 << 20, &NoCtl).unwrap();
        prime_open_writes(&mut f, &mut live, &StopCtl); // e.g. paused at once
        assert!(live.changes.open_unknown());
        assert_eq!(live.changes.lowest_dropped_usn(), Some(open_usn));
        let live = Arc::new(RwLock::new(live));
        if wrapped {
            w.lock().unwrap().first_usn = open_usn + 1;
        }
        let completed = reprime_open_writes(&mut f, &live, &NoCtl);
        let g = read_index(&live);
        let open: Vec<u64> = g.changes.open_writes().map(|(r, _)| r).collect();
        if wrapped {
            assert!(!completed);
            assert!(g.changes.open_unknown(), "stays until rebuild");
            assert!(open.is_empty());
        } else {
            assert!(completed);
            assert!(!g.changes.open_unknown(), "recovered");
            assert_eq!(open, vec![22], "and the session is tracked");
        }
    }
}

/// FER-503 P3: a long re-prime runs a few reads per call (per worker pass)
/// and resumes where it stopped.
#[test]
fn a_reprime_is_resumable_in_small_steps() {
    let w = Arc::new(Mutex::new(World::new()));
    {
        let mut ww = w.lock().unwrap();
        ww.per_read = 1;
        for rec in 100..110u64 {
            ww.change(rec, file(1, ROOT, "x", 1, 1), reason::DATA_OVERWRITE);
        }
    }
    let mut f = fake(&w);
    let live = build(&mut f, &Scope::everything(), 64 << 20, &NoCtl).unwrap();
    let live = Arc::new(RwLock::new(live));
    write_index(&live).changes.set_open_unknown();
    let mut scan = reprime_begin(&mut f, &live).unwrap();
    let mut calls = 0;
    let progress = loop {
        calls += 1;
        match reprime_advance(&mut f, &live, &mut scan, &NoCtl, 3) {
            ReprimeProgress::Running => {
                assert!(read_index(&live).changes.open_unknown(), "not yet");
            }
            p => break p,
        }
    };
    assert_eq!(progress, ReprimeProgress::Completed { recovered: true });
    assert!(calls >= 4, "10 reads, 3 per call: {calls}");
    let g = read_index(&live);
    assert!(!g.changes.open_unknown());
    assert_eq!(g.changes.open_writes().count(), 10);
}

// ── FER-503 review 2: the journal follow runs between re-prime passes ────

/// A live, primed index whose log is `open_unknown` (no known USN), with
/// the journal holding `setup`'s records below the index position.
fn unknown_live_index(
    w: &Arc<Mutex<World>>,
    setup: impl FnOnce(&mut World),
) -> (Fake, SharedIndex) {
    let mut f = fake(w);
    let mut live = build(&mut f, &Scope::everything(), 64 << 20, &NoCtl).unwrap();
    prime_open_writes(&mut f, &mut live, &NoCtl);
    let live = Arc::new(RwLock::new(live));
    setup(&mut w.lock().unwrap());
    follow(&mut f, &live);
    write_index(&live).changes.set_open_unknown();
    (f, live)
}

fn tracked(live: &SharedIndex) -> Vec<u64> {
    let mut v: Vec<u64> = read_index(live)
        .changes
        .open_writes()
        .map(|(r, _)| r)
        .collect();
    v.sort();
    v
}

/// Reviewer's case: 22 opened and closed below the target, not yet read by
/// the first pass; between passes 22 is opened again and the follow tracks
/// it. The replayed old `CLOSE` must not untrack it.
#[test]
fn a_replayed_close_never_untracks_a_session_the_follow_reopened() {
    let w = Arc::new(Mutex::new(World::new()));
    w.lock().unwrap().per_read = 1;
    let (mut f, live) = unknown_live_index(&w, |ww| {
        for _ in 0..3 {
            ww.change(
                20,
                file(1, ROOT, "a.txt", 100, 7),
                reason::BASIC_INFO_CHANGE,
            );
        }
        let doc = file(1, fref(21, 1), "report.doc", 50, 9);
        ww.change(22, doc.clone(), reason::DATA_OVERWRITE);
        ww.change(22, doc, reason::DATA_OVERWRITE | reason::CLOSE);
    });
    let mut scan = reprime_begin(&mut f, &live).unwrap();
    assert_eq!(
        reprime_advance(&mut f, &live, &mut scan, &NoCtl, 1),
        ReprimeProgress::Running,
        "the first pass reads one filler record"
    );
    // Between passes: 22 is opened again; the follow tracks it.
    w.lock().unwrap().change(
        22,
        file(1, fref(21, 1), "report.doc", 60, 10),
        reason::DATA_OVERWRITE,
    );
    follow(&mut f, &live);
    assert_eq!(tracked(&live), vec![22]);
    let progress = loop {
        match reprime_advance(&mut f, &live, &mut scan, &NoCtl, 1) {
            ReprimeProgress::Running => follow(&mut f, &live),
            p => break p,
        }
    };
    assert_eq!(progress, ReprimeProgress::Completed { recovered: true });
    assert!(!read_index(&live).changes.open_unknown());
    assert_eq!(
        tracked(&live),
        vec![22],
        "the reopened session stays tracked"
    );
}

/// The other direction: 22 opened below the target and closed by the
/// follow while the scan runs. The replay must not add it back (an entry
/// that would be reported changed forever and count against the cap).
#[test]
fn a_session_the_follow_saw_close_is_not_re_added_by_the_replay() {
    let w = Arc::new(Mutex::new(World::new()));
    w.lock().unwrap().per_read = 1;
    let (mut f, live) = unknown_live_index(&w, |ww| {
        ww.change(
            22,
            file(1, fref(21, 1), "report.doc", 50, 9),
            reason::DATA_OVERWRITE,
        );
    });
    // The open session is not tracked: the flag hides it.
    write_index(&live).changes = {
        let mut l = ChangeLog::new(read_index(&live).changes.floor());
        l.set_open_unknown();
        l
    };
    let mut scan = reprime_begin(&mut f, &live).unwrap();
    w.lock().unwrap().change(
        22,
        file(1, fref(21, 1), "report.doc", 60, 10),
        reason::DATA_OVERWRITE | reason::CLOSE,
    );
    follow(&mut f, &live);
    let progress = reprime_advance(&mut f, &live, &mut scan, &NoCtl, 64);
    assert_eq!(progress, ReprimeProgress::Completed { recovered: true });
    assert!(tracked(&live).is_empty(), "no ghost session");
}

/// The merge itself overflows: the follow filled the cap while the scan
/// ran, and the replay found one more open session. The flag stays, and
/// that session's USN is the new lower bound.
#[test]
fn a_merge_beyond_the_cap_keeps_open_unknown() {
    let w = Arc::new(Mutex::new(World::new()));
    w.lock().unwrap().per_read = 512;
    let open_usn = w.lock().unwrap().next_usn;
    let (mut f, live) = unknown_live_index(&w, |ww| {
        ww.change(
            22,
            file(1, fref(21, 1), "report.doc", 50, 9),
            reason::DATA_OVERWRITE,
        );
    });
    write_index(&live).changes = {
        let mut l = ChangeLog::new(read_index(&live).changes.floor());
        l.set_open_unknown();
        l
    };
    let mut scan = reprime_begin(&mut f, &live).unwrap();
    {
        let mut ww = w.lock().unwrap();
        for rec in 100..(100 + MAX_OPEN_WRITES as u64) {
            ww.change(rec, file(1, ROOT, "x", 1, 1), reason::DATA_OVERWRITE);
        }
    }
    follow(&mut f, &live);
    assert_eq!(read_index(&live).changes.open_overflows(), 0, "fits");
    let progress = reprime_advance(&mut f, &live, &mut scan, &NoCtl, 64);
    assert_eq!(progress, ReprimeProgress::Completed { recovered: false });
    let g = read_index(&live);
    assert!(g.changes.open_unknown(), "the merge overflowed");
    assert_eq!(g.changes.lowest_dropped_usn(), Some(open_usn));
    assert_eq!(g.changes.open_writes().count(), MAX_OPEN_WRITES);
    assert!(!g.changes.open_writes().any(|(r, _)| r == 22));
}
