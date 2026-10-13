// FER-486 A1: shared read-path guard. Every spelling from the security review table is refused,
// a test pipe never sees a connection, and normal drive and UNC paths keep working.
use super::*;

#[test]
fn device_and_nt_namespaces_are_refused_in_every_spelling() {
    for path in [
        r"\\.\pipe\x",
        "//./pipe/x",
        r"\\?\C:\Windows\win.ini",
        "//?/C:/Windows/win.ini",
        r"/\?\C:\Windows\win.ini",
        r"\/?/C:\Windows\win.ini",
        r"\??\C:\Windows\win.ini",
        "/??/C:/Windows/win.ini",
        r"\\.\PhysicalDrive0",
        r"\\?\UNC\server\share\x.txt",
        r"\\.",
        r"\\?",
    ] {
        assert!(check_str(path).is_err(), "{path}");
    }
}

#[test]
fn alternate_data_streams_and_relative_paths_are_refused() {
    for path in [
        r"C:\data\notes.txt:secret",
        r"C:\data\notes.txt::$DATA",
        "C:/data/notes.txt:secret",
        r"\\server\share\notes.txt:secret",
        r"C:notes.txt",
        r"notes.txt",
        r"..\notes.txt",
        r"\Windows\win.ini",
        "",
        "C:\\a\u{0007}b.txt",
    ] {
        assert!(check_str(path).is_err(), "{path:?}");
    }
}

#[test]
fn pipe_and_mailslot_shares_are_refused_but_normal_unc_stays_allowed() {
    for path in [
        r"\\localhost\pipe\x",
        "//localhost/PIPE/x",
        r"\\server\mailslot\x",
        r"\\server",
        r"\\server\",
        r"\\.host\share\x",
    ] {
        assert!(check_str(path).is_err(), "{path}");
    }
    assert_eq!(
        check_str(r"\\fileserver\team\docs\a.txt").unwrap(),
        r"\\fileserver\team\docs\a.txt"
    );
    assert_eq!(
        check_str("//fileserver/team/docs/a.txt").unwrap(),
        r"\\fileserver\team\docs\a.txt"
    );
    assert_eq!(check_str(r"\\nas-01\c$\x").unwrap(), r"\\nas-01\c$\x");
    assert_eq!(
        check_str("C:/Users/demo/a.txt").unwrap(),
        r"C:\Users\demo\a.txt"
    );
    assert_eq!(check_str(r"D:\").unwrap(), r"D:\");
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_PIPE_LISTENING, GetLastError, HANDLE, INVALID_HANDLE_VALUE,
    };
    use windows_sys::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
    use windows_sys::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_NOWAIT, PIPE_TYPE_BYTE,
    };

    static SEQ: AtomicU64 = AtomicU64::new(0);

    struct Pipe {
        name: String,
        handle: HANDLE,
    }
    impl Pipe {
        fn new() -> Self {
            let name = format!(
                "ferrum-fer486-guard-{}-{}",
                std::process::id(),
                SEQ.fetch_add(1, Ordering::SeqCst)
            );
            let wide: Vec<u16> = format!(r"\\.\pipe\{name}")
                .encode_utf16()
                .chain([0])
                .collect();
            // SAFETY: valid NUL-terminated name; default security; checked below.
            let handle = unsafe {
                CreateNamedPipeW(
                    wide.as_ptr(),
                    PIPE_ACCESS_DUPLEX,
                    PIPE_TYPE_BYTE | PIPE_NOWAIT,
                    1,
                    0,
                    0,
                    0,
                    std::ptr::null(),
                )
            };
            assert_ne!(handle, INVALID_HANDLE_VALUE);
            Self { name, handle }
        }
        /// True while no client ever connected (non-blocking ConnectNamedPipe says "listening").
        fn untouched(&self) -> bool {
            // SAFETY: valid pipe handle; PIPE_NOWAIT makes this return immediately.
            let ok = unsafe { ConnectNamedPipe(self.handle, std::ptr::null_mut()) };
            ok == 0 && unsafe { GetLastError() } == ERROR_PIPE_LISTENING
        }
    }
    impl Drop for Pipe {
        fn drop(&mut self) {
            // SAFETY: handle owned by this struct.
            unsafe { CloseHandle(self.handle) };
        }
    }

    #[test]
    fn a_test_pipe_never_receives_a_connection_through_preview_or_attach() {
        let pipe = Pipe::new();
        let n = &pipe.name;
        let forms = [
            format!(r"\\.\pipe\{n}"),
            format!("//./pipe/{n}"),
            format!(r"/\.\pipe\{n}"),
            format!(r"\\?\pipe\{n}"),
            format!("//?/pipe/{n}"),
            format!(r"\\localhost\pipe\{n}"),
            format!("//127.0.0.1/pipe/{n}"),
        ];
        for path in &forms {
            assert!(open_entry(path).is_err(), "{path}");
            assert!(crate::preview::read_preview(path, 1024).is_err(), "{path}");
            assert!(
                crate::preview_image::read_image_preview(path).is_err(),
                "{path}"
            );
            let inspected = crate::ai::attach::inspect(std::slice::from_ref(path)).unwrap();
            assert!(inspected[0].info.is_none(), "{path}");
            let extracted = crate::ai::attach::extract(&[crate::ai::attach::AttachmentRef {
                path: path.clone(),
                confirmed_sensitive: true,
            }]);
            assert!(extracted[0].body.is_none(), "{path}");
        }
        assert!(pipe.untouched(), "the pipe server saw a connection");
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ferrum-fer486-guard-{tag}-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read_all(file: &GuardedFile) -> String {
        use std::io::Read;
        let mut text = String::new();
        file.open_read().unwrap().read_to_string(&mut text).unwrap();
        text
    }

    /// Defence in depth: even if a pipe path got past the string check, the handle-level check
    /// (identification-only SQOS + GetFileType) refuses it.
    #[test]
    fn the_handle_check_refuses_pipes_and_character_devices() {
        let pipe = Pipe::new();
        let handle = open_flags(&format!(r"\\.\pipe\{}", pipe.name), false).unwrap();
        assert!(require_disk(&handle).is_err());
        let nul = std::env::temp_dir().join("NUL");
        let nul = nul.to_string_lossy();
        assert!(check_str(&nul).is_ok());
        assert!(open_entry(&nul).is_err(), "NUL is a character device");
        assert!(crate::preview::read_preview(&nul, 16).is_err());
    }

    #[test]
    fn the_ipc_share_is_refused() {
        for path in [
            r"\\localhost\IPC$",
            r"\\server\ipc$\x",
            "//server/Ipc$/srvsvc",
        ] {
            assert!(check_str(path).is_err(), "{path}");
            assert!(open_entry(path).is_err(), "{path}");
        }
    }

    /// B1: what is read is the handle that was checked. Swapping the path for another file after
    /// the check changes nothing for the guarded reader.
    #[test]
    fn the_read_uses_the_checked_handle_even_if_the_path_is_swapped() {
        let dir = temp_dir("swap");
        let file = dir.join("a.txt");
        std::fs::write(&file, "checked content").unwrap();
        let Entry::File(guarded) = open_entry(file.to_str().unwrap()).unwrap() else {
            panic!("expected a file");
        };
        std::fs::rename(&file, dir.join("moved.txt")).unwrap();
        std::fs::write(&file, "SWAPPED IN").unwrap();
        assert_eq!(read_all(&guarded), "checked content");
        assert_eq!(guarded.meta.len(), 15);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// FER-488: a path-only backend (unrar) gets the pinned file object, and only that one. A
    /// file swapped in between the check and the pin is refused; while pinned, the file can be
    /// neither replaced nor renamed, and the handed-out path is the real (final) location.
    #[test]
    fn the_pin_for_a_path_only_backend_holds_the_checked_file() {
        let dir = temp_dir("pin");
        let file = dir.join("a.rar");
        std::fs::write(&file, "checked archive").unwrap();
        let Entry::File(guarded) = open_entry(file.to_str().unwrap()).unwrap() else {
            panic!("expected a file");
        };
        let pin = guarded.pin_for_path_api().unwrap();
        assert!(pin.path.starts_with(r"\\?\"), "{}", pin.path);
        assert!(pin.path.to_lowercase().ends_with(r"\a.rar"), "{}", pin.path);
        assert_eq!(
            std::fs::read_to_string(&pin.path).unwrap(),
            "checked archive"
        );
        assert!(std::fs::rename(&file, dir.join("moved.rar")).is_err());
        assert!(std::fs::write(&file, "SWAPPED IN").is_err());
        assert!(std::fs::rename(&dir, dir.with_extension("moved")).is_err());
        drop(pin);

        // Swapped after the check, before the pin: refused (different file object).
        std::fs::rename(&file, dir.join("moved.rar")).unwrap();
        std::fs::write(&file, "SWAPPED IN").unwrap();
        let err = guarded
            .pin_for_path_api()
            .err()
            .expect("swap must be refused");
        assert_eq!(err.code, ErrorCode::InvalidPath);
        drop(guarded);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// B1: a folder is listed through its checked handle; a folder swapped in afterwards is not.
    #[test]
    fn a_folder_is_listed_through_the_checked_handle() {
        let root = temp_dir("dir");
        let dir = root.join("d");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("one.txt"), "12345").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        let Entry::Dir(guarded) = open_entry(dir.to_str().unwrap()).unwrap() else {
            panic!("expected a folder");
        };
        let (rows, counted, more) = guarded.list(10, 100).unwrap();
        let mut names: Vec<(String, bool, u64)> = rows
            .into_iter()
            .map(|r| (r.name, r.is_dir, r.size))
            .collect();
        names.sort();
        assert_eq!(
            names,
            vec![("one.txt".into(), false, 5), ("sub".into(), true, 0)]
        );
        assert_eq!((counted, more), (2, false));
        let (rows, counted, more) = guarded.list(1, 1).unwrap();
        assert_eq!((rows.len(), counted, more), (1, 1, true));

        // Swap the folder for another one after the check: the listing refuses instead of
        // reading the intruder (the listing handle must be the checked folder).
        std::fs::rename(&dir, root.join("moved")).unwrap();
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("intruder.txt"), "x").unwrap();
        let error = guarded.list(10, 100).err().expect("swapped folder refused");
        assert!(error.message.contains("changed"), "{}", error.message);
        let _ = std::fs::remove_dir_all(root);
    }

    /// B1: a file symlink pointing at a pipe is refused on the followed (checked) handle. Needs the
    /// symlink privilege (Developer Mode or admin); skipped otherwise.
    #[test]
    fn a_symlink_to_a_pipe_is_refused_on_the_followed_handle() {
        let pipe = Pipe::new();
        let dir = temp_dir("link");
        let link = dir.join("link.txt");
        match std::os::windows::fs::symlink_file(format!(r"\\.\pipe\{}", pipe.name), &link) {
            Ok(()) => {
                assert!(open_entry(link.to_str().unwrap()).is_err());
                assert!(crate::preview::read_preview(link.to_str().unwrap(), 16).is_err());
            }
            Err(e) => eprintln!("skip symlink check: {e}"),
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn drive_files_folders_and_unc_paths_pass_the_guard() {
        let dir = temp_dir("ok");
        let file = dir.join("a.txt");
        std::fs::write(&file, "hello").unwrap();
        assert!(matches!(
            open_entry(dir.to_str().unwrap()).unwrap(),
            Entry::Dir(_)
        ));
        let forward = file.to_string_lossy().replace('\\', "/");
        let Entry::File(guarded) = open_entry(&forward).unwrap() else {
            panic!("expected a file");
        };
        assert_eq!(guarded.path, file.to_string_lossy());
        assert_eq!(crate::preview::read_preview(&forward, 64).unwrap().size, 5);

        // A real UNC read through the administrative share, when this machine allows it.
        let local = file.to_string_lossy().into_owned();
        let unc = format!(r"\\localhost\{}$\{}", &local[..1], &local[3..]);
        match open_entry(&unc) {
            Ok(Entry::File(guarded)) => {
                assert_eq!(read_all(&guarded), "hello");
                assert_eq!(crate::preview::read_preview(&unc, 64).unwrap().size, 5);
                let out = crate::ai::attach::extract(&[crate::ai::attach::AttachmentRef {
                    path: unc.clone(),
                    confirmed_sensitive: false,
                }]);
                assert_eq!(out[0].body.as_deref(), Some("hello"));
            }
            Ok(_) => panic!("UNC file is not a file"),
            Err(e) => eprintln!("skip live UNC check ({unc}): {}", e.message),
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}

// FER-467 R7: local settings locations.
#[test]
fn local_locations_refuse_devices_streams_unc_and_link_tags() {
    for name in [
        "CON",
        "con.json",
        "Nul .txt",
        "COM1",
        "lpt9.json",
        "COM¹",
        "CONOUT$",
    ] {
        assert!(is_reserved_name(name), "{name}");
    }
    for name in ["console", "COM10", "LPT", "nul-list.json", "aux2"] {
        assert!(!is_reserved_name(name), "{name}");
    }
    assert_eq!(
        check_local_str(r"\\srv\share\h.json"),
        Err(LocalRefusal::Network)
    );
    assert_eq!(
        check_local_str(r"/\srv\share\h.json"),
        Err(LocalRefusal::Network)
    );
    assert_eq!(
        check_local_str(r"\\?\C:\h.json"),
        Err(LocalRefusal::Invalid)
    );
    assert_eq!(check_local_str(r"C:\a\h.json:x"), Err(LocalRefusal::Stream));
    assert_eq!(
        check_local_str(r"C:\a\nul\h.json"),
        Err(LocalRefusal::Reserved)
    );
    assert_eq!(
        check_local_str(r"C:\a.\h.json"),
        Err(LocalRefusal::Reserved)
    );
    assert_eq!(check_local_str(r"C:\a\h*.json"), Err(LocalRefusal::Invalid));
    assert_eq!(
        check_local_str("C:/a/h.json").as_deref(),
        Ok(r"C:\a\h.json")
    );
    // Junction (MOUNT_POINT) and symlink tags redirect; cloud placeholder tags do not.
    assert!(reparse_tag_redirects(0xA000_0003));
    assert!(reparse_tag_redirects(0xA000_000C));
    assert!(!reparse_tag_redirects(0x9000_001A));
    assert!(!reparse_tag_redirects(0x8000_001B));
}
