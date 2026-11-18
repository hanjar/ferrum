use super::*;

fn item(path: &str, ms: i64, source: &'static str) -> RecentFile {
    RecentFile {
        path: path.to_owned(),
        name: display_name(path),
        is_dir: Some(false),
        last_used_ms: ms,
        source,
        label: source.to_owned(),
    }
}

fn s(v: &str) -> Vec<String> {
    vec![v.to_owned()]
}

#[test]
fn sanitize_accepts_drive_and_unc_paths() {
    assert_eq!(
        sanitize_abs_path("C:\\Users\\a\\x.txt", &[]).as_deref(),
        Some("C:\\Users\\a\\x.txt")
    );
    assert_eq!(
        sanitize_abs_path("c:/Users/a/x.txt", &[]).as_deref(),
        Some("c:\\Users\\a\\x.txt")
    );
    assert_eq!(
        sanitize_abs_path("\\\\srv\\share\\dir\\", &[]).as_deref(),
        Some("\\\\srv\\share\\dir")
    );
    assert_eq!(sanitize_abs_path("D:\\", &[]).as_deref(), Some("D:\\"));
}

#[test]
fn sanitize_rejects_unsafe_targets() {
    for bad in [
        "",
        "   ",
        "relative\\file.txt",
        ".\\x",
        "C:file.txt",
        "\\\\?\\C:\\x",
        "\\\\.\\PhysicalDrive0",
        "//?/C:/x",
        "\\\\server",
        "\\\\server\\",
        "shell:recent",
        "SHELL:Downloads",
        "::{20D04FE0-3AEA-1069-A2D8-08002B30309D}",
        "C:\\a\\..\\b",
        "C:\\a\\file.txt:stream",
        "http://example.com/x",
        "C:\\%TEMP%\\x",
        "C:\\a\\b*.txt",
        "C:\\a\u{0}b",
    ] {
        assert_eq!(
            sanitize_abs_path(bad, &[]),
            None,
            "{bad:?} must be rejected"
        );
    }
    let long = format!("C:\\{}", "a".repeat(MAX_TARGET_LEN));
    assert_eq!(sanitize_abs_path(&long, &[]), None);
}

#[test]
fn sanitize_rejects_blocked_roots_case_insensitively() {
    let blocked = s("C:\\Users\\a\\AppData\\Roaming\\dev.ferrum.app");
    assert_eq!(
        sanitize_abs_path(
            "c:\\users\\a\\appdata\\roaming\\DEV.FERRUM.APP\\config.json",
            &blocked
        ),
        None
    );
    assert_eq!(
        sanitize_abs_path("C:\\Users\\a\\AppData\\Roaming\\dev.ferrum.app", &blocked),
        None
    );
    // A sibling with the same prefix is not inside.
    assert!(
        sanitize_abs_path(
            "C:\\Users\\a\\AppData\\Roaming\\dev.ferrum.app2\\x",
            &blocked
        )
        .is_some()
    );
}

#[test]
fn path_under_respects_component_boundary() {
    assert!(path_under("C:\\Work\\a", "C:\\work"));
    assert!(path_under("C:\\Work", "c:\\work\\"));
    assert!(!path_under("C:\\Workshop", "C:\\Work"));
    assert!(!path_under("C:\\x", ""));
}

#[test]
fn display_name_is_last_component() {
    assert_eq!(display_name("C:\\a\\b.txt"), "b.txt");
    assert_eq!(display_name("C:\\a\\dir"), "dir");
    assert_eq!(display_name("C:\\"), "C:\\");
    assert_eq!(display_name("\\\\srv\\share"), "share");
}

#[test]
fn pick_newest_sorts_and_caps() {
    let c = |p: &str, ms| LnkCandidate {
        path: PathBuf::from(p),
        modified_ms: ms,
    };
    let picked = pick_newest(vec![c("b", 10), c("a", 30), c("c", 20), c("d", 30)], 3);
    let names: Vec<_> = picked.iter().map(|p| p.path.to_str().unwrap()).collect();
    assert_eq!(names, ["a", "d", "c"]); // ties by path
    let many: Vec<_> = (0..500).map(|i| c(&format!("f{i}"), i)).collect();
    assert_eq!(pick_newest(many, 10_000).len(), MAX_LIMIT);
}

#[test]
fn dedupe_keeps_newest_case_insensitive() {
    let out = dedupe_newest(vec![
        item("C:\\A\\x.txt", 10, "windows"),
        item("c:\\a\\X.TXT", 50, "new"),
        item("C:\\a\\y.txt", 30, "windows"),
    ]);
    assert_eq!(out.len(), 2);
    assert_eq!(out[0].last_used_ms, 50);
    assert_eq!(out[0].source, "new");
    assert_eq!(out[1].path, "C:\\a\\y.txt");
}

#[test]
fn window_bounds_and_clamp() {
    let day = 86_400_000;
    let now = 100 * day;
    assert!(within_window(now - 6 * day, now, 7));
    assert!(!within_window(now - 8 * day, now, 7));
    assert!(within_window(now + day / 2, now, 7)); // small clock skew
    assert!(!within_window(now + 3 * day, now, 7));
    // 0 days clamps to 1, huge clamps to 365.
    assert!(!within_window(now - 2 * day, now, 0));
    assert!(within_window(now - 300 * day, now, 9999));
}

#[test]
fn noise_names() {
    for n in [
        "desktop.ini",
        "Desktop.INI",
        "Thumbs.db",
        "~$doc.docx",
        "a.crdownload",
        "b.PART",
    ] {
        assert!(is_noise_name(n), "{n}");
    }
    assert!(!is_noise_name("report.docx"));
}

#[test]
fn sort_and_cap_orders_newest_first() {
    let items = vec![
        item("C:\\a", 1, "new"),
        item("C:\\b", 3, "new"),
        item("C:\\c", 2, "new"),
    ];
    let out = sort_and_cap(items, 2);
    assert_eq!(
        out.iter().map(|i| i.last_used_ms).collect::<Vec<_>>(),
        [3, 2]
    );
}

#[test]
fn folder_label_downloads_or_name() {
    assert_eq!(
        folder_label("C:\\Users\\a\\Downloads", Some("c:\\users\\a\\downloads\\")),
        "Downloads"
    );
    assert_eq!(
        folder_label("D:\\Work\\Proj", Some("C:\\Users\\a\\Downloads")),
        "Proj"
    );
    assert_eq!(folder_label("D:\\Work\\Proj", None), "Proj");
}

#[test]
fn watch_folder_validation_filters_dedupes_and_caps() {
    let blocked = s("C:\\Ferrum");
    let raw: Vec<String> = [
        "C:\\Users\\a\\Downloads",
        "c:\\users\\a\\downloads\\",
        "relative",
        "\\\\?\\C:\\x",
        "C:\\Ferrum\\data",
        "D:\\Work",
    ]
    .iter()
    .map(|x| (*x).to_owned())
    .collect();
    let out = validate_watch_folders(&raw, &blocked);
    assert_eq!(out, ["C:\\Users\\a\\Downloads", "D:\\Work"]);
    let many: Vec<String> = (0..40).map(|i| format!("C:\\d{i}")).collect();
    assert_eq!(validate_watch_folders(&many, &[]).len(), MAX_FOLDERS);
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ferrum-recent-{tag}-{}-{}",
        std::process::id(),
        now_ms()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // CI runners give TEMP in 8.3 short form (C:\Users\RUNNER~1\...), but link targets read back
    // in long form: work in the long form so path comparisons in these tests mean what they say.
    long_form(&dir.to_string_lossy())
        .map(PathBuf::from)
        .unwrap_or(dir)
}

#[test]
fn scan_new_items_lists_top_level_only_and_filters() {
    let dir = temp_dir("scan");
    std::fs::write(dir.join("fresh.txt"), "x").unwrap();
    std::fs::write(dir.join("desktop.ini"), "x").unwrap();
    std::fs::write(dir.join("part.crdownload"), "x").unwrap();
    std::fs::create_dir(dir.join("newdir")).unwrap();
    std::fs::write(dir.join("newdir").join("inner.txt"), "x").unwrap();
    let items = scan_new_items(dir.to_str().unwrap(), "Downloads", now_ms(), 7, &[]);
    let mut names: Vec<_> = items.iter().map(|i| i.name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["fresh.txt", "newdir"]);
    assert!(
        items
            .iter()
            .all(|i| i.source == "new" && i.label == "Downloads")
    );
    assert_eq!(
        items.iter().find(|i| i.name == "newdir").unwrap().is_dir,
        Some(true)
    );
    // An old window (item created "in the future" relative to now - 10 days) is excluded.
    let far_future = now_ms() + 30 * 86_400_000;
    assert!(scan_new_items(dir.to_str().unwrap(), "x", far_future, 1, &[]).is_empty());
    // A blocked root hides everything.
    let blocked = vec![dir.to_string_lossy().into_owned()];
    assert!(scan_new_items(dir.to_str().unwrap(), "x", now_ms(), 7, &blocked).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scan_watched_folders_skips_missing_folder_and_merges() {
    let a = temp_dir("wa");
    std::fs::write(a.join("one.txt"), "x").unwrap();
    let missing = a.join("does-not-exist");
    let out = scan_watched_folders(
        vec![
            (a.to_string_lossy().into_owned(), "A".to_owned()),
            (missing.to_string_lossy().into_owned(), "M".to_owned()),
        ],
        7,
        50,
        Vec::new(),
        Some(Duration::from_secs(5)),
    );
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].name, "one.txt");
    let _ = std::fs::remove_dir_all(&a);
}

#[test]
fn newest_lnk_files_ignores_other_files_and_subfolders() {
    let dir = temp_dir("lnkenum");
    std::fs::write(dir.join("a.lnk"), "x").unwrap();
    std::fs::write(dir.join("b.LNK"), "x").unwrap();
    std::fs::write(dir.join("c.txt"), "x").unwrap();
    std::fs::create_dir(dir.join("AutomaticDestinations")).unwrap();
    std::fs::write(dir.join("AutomaticDestinations").join("d.lnk"), "x").unwrap();
    std::fs::create_dir(dir.join("dir.lnk")).unwrap();
    let found = newest_lnk_files(&dir, 10);
    let mut names: Vec<_> = found
        .iter()
        .map(|c| c.path.file_name().unwrap().to_string_lossy().to_lowercase())
        .collect();
    names.sort();
    assert_eq!(names, ["a.lnk", "b.lnk"]);
    assert_eq!(newest_lnk_files(&dir, 1).len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(windows)]
#[test]
fn windows_recent_roundtrip_with_real_lnk_files() {
    let dir = temp_dir("lnkreal");
    let target_dir = temp_dir("lnktargets");
    let file_target = target_dir.join("report.docx");
    std::fs::write(&file_target, "x").unwrap();
    let folder_target = target_dir.join("Projects");
    std::fs::create_dir(&folder_target).unwrap();
    let blocked_dir = temp_dir("lnkblocked");
    let blocked_file = blocked_dir.join("secret.json");
    std::fs::write(&blocked_file, "x").unwrap();

    let created = com::with_apartment(|| {
        com::write_link(&dir.join("report.docx.lnk"), file_target.to_str().unwrap())
            && com::write_link(&dir.join("Projects.lnk"), folder_target.to_str().unwrap())
            && com::write_link(&dir.join("secret.json.lnk"), blocked_file.to_str().unwrap())
    });
    assert_eq!(created, Some(true), "could not create .lnk files");
    std::fs::write(dir.join("garbage.lnk"), b"not a shortcut").unwrap();

    let blocked = expand_blocked_roots(vec![blocked_dir.to_string_lossy().into_owned()]);
    let items = read_windows_recent(&dir, &blocked, 50);
    let mut names: Vec<_> = items.iter().map(|i| i.name.clone()).collect();
    names.sort();
    assert_eq!(names, ["Projects", "report.docx"], "{items:?}");
    let file = items.iter().find(|i| i.name == "report.docx").unwrap();
    assert_eq!(file.source, "windows");
    assert!(
        file.path
            .eq_ignore_ascii_case(file_target.to_str().unwrap())
    );
    assert!(file.last_used_ms > 0);
    let folder = items.iter().find(|i| i.name == "Projects").unwrap();
    assert_eq!(folder.is_dir, Some(true));

    // The limit applies BEFORE resolving (the newest .lnk may be the unreadable one).
    std::fs::remove_file(dir.join("garbage.lnk")).unwrap();
    assert_eq!(read_windows_recent(&dir, &[], 1).len(), 1);
    for d in [&dir, &target_dir, &blocked_dir] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[cfg(windows)]
#[test]
fn real_recent_folder_is_readable_and_filtered() {
    // Smoke against the machine's own Recent folder: must not panic or hang, every item valid.
    let Some(dir) = windows_recent_dir() else {
        return;
    };
    let all = read_windows_recent(&dir, &[], MAX_LIMIT);
    eprintln!("real recent items: {}", all.len());
    for item in all {
        assert!(sanitize_abs_path(&item.path, &[]).is_some(), "{item:?}");
        assert_eq!(item.source, "windows");
    }
}

#[cfg(windows)]
fn short_form(path: &Path) -> Option<String> {
    use windows_sys::Win32::Storage::FileSystem::GetShortPathNameW;
    let wide: Vec<u16> = path
        .to_string_lossy()
        .encode_utf16()
        .chain(Some(0))
        .collect();
    let mut buf = vec![0u16; 4096];
    let n =
        unsafe { GetShortPathNameW(wide.as_ptr(), buf.as_mut_ptr(), buf.len() as u32) } as usize;
    (n > 0 && n < buf.len()).then(|| String::from_utf16_lossy(&buf[..n]))
}

#[cfg(windows)]
#[test]
fn blocked_root_in_8_3_short_form_still_filters_long_form_targets() {
    // A directory whose long name forces a distinct 8.3 alias, so the short and long forms differ.
    let base = temp_dir("shortroot");
    let blocked_dir = base.join("Ferrum Blocked Directory Long Name");
    std::fs::create_dir(&blocked_dir).unwrap();
    let blocked_file = blocked_dir.join("secret.json");
    std::fs::write(&blocked_file, "x").unwrap();
    let Some(short) = short_form(&blocked_dir) else {
        let _ = std::fs::remove_dir_all(&base);
        return; // 8.3 name generation is disabled on this volume: nothing to test
    };
    if norm_key(&short) == norm_key(blocked_dir.to_str().unwrap()) {
        let _ = std::fs::remove_dir_all(&base);
        return;
    }
    let dir = temp_dir("shortlnk");
    let created = com::with_apartment(|| {
        com::write_link(&dir.join("secret.json.lnk"), blocked_file.to_str().unwrap())
    });
    assert_eq!(created, Some(true));
    // Unexpanded short root would not match; expanded it must.
    let blocked = expand_blocked_roots(vec![short.clone()]);
    assert!(blocked.len() == 2, "{blocked:?}");
    assert!(read_windows_recent(&dir, &blocked, 50).is_empty());
    // The long-form root is unchanged (only the original is kept).
    let long = expand_blocked_roots(vec![blocked_dir.to_string_lossy().into_owned()]);
    assert_eq!(long.len(), 1);
    assert!(read_windows_recent(&dir, &long, 50).is_empty());
    for d in [&dir, &base] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
fn expand_blocked_roots_keeps_originals_and_ignores_missing_or_non_drive_roots() {
    let roots = vec![
        r"C:\definitely\missing\dir".to_owned(),
        r"\srv\share".to_owned(),
    ];
    assert_eq!(expand_blocked_roots(roots.clone()), roots);
}
