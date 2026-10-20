use super::*;

fn s(p: &str) -> String {
    p.to_string()
}

/// Builds a raw block by hand so tests do not depend on the encoder.
fn raw(p_files: u32, wide: u32, units: &[u16]) -> Vec<u8> {
    let mut v = Vec::new();
    v.extend_from_slice(&p_files.to_le_bytes());
    v.extend_from_slice(&0i32.to_le_bytes());
    v.extend_from_slice(&0i32.to_le_bytes());
    v.extend_from_slice(&0u32.to_le_bytes());
    v.extend_from_slice(&wide.to_le_bytes());
    for u in units {
        v.extend_from_slice(&u.to_le_bytes());
    }
    v
}

fn units(text: &str) -> Vec<u16> {
    text.encode_utf16().collect()
}

#[test]
fn round_trip_single_and_multiple_paths() {
    for set in [
        vec![s(r"C:\a\b.txt")],
        vec![s(r"C:\a\b.txt"), s(r"D:\x y\z"), s(r"\\srv\share\f.bin")],
    ] {
        let bytes = encode_dropfiles(&set).unwrap();
        assert_eq!(parse_dropfiles(&bytes).unwrap(), set);
    }
}

#[test]
fn encoder_header_is_wide_with_double_nul() {
    let bytes = encode_dropfiles(&[s(r"C:\a")]).unwrap();
    assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 20);
    assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 1);
    assert_eq!(&bytes[bytes.len() - 4..], &[0, 0, 0, 0]);
    assert_eq!(bytes.len(), 20 + (4 + 1 + 1) * 2);
}

#[test]
fn unicode_and_long_paths_round_trip() {
    let uni = s("C:\\data\\berkas \u{4e2d}\u{6587} \u{1F600}.txt");
    let long = format!("C:\\{}", "d".repeat(MAX_PATH_UNITS - 3));
    let set = vec![uni, long];
    assert_eq!(
        parse_dropfiles(&encode_dropfiles(&set).unwrap()).unwrap(),
        set
    );
}

#[test]
fn encoder_refuses_bad_input() {
    assert_eq!(encode_dropfiles(&[]), Err(DropFilesError::Empty));
    assert_eq!(encode_dropfiles(&[s("")]), Err(DropFilesError::Empty));
    assert_eq!(
        encode_dropfiles(&[s("C:\\a\0b")]),
        Err(DropFilesError::NulInPath)
    );
    let too_long = format!("C:\\{}", "d".repeat(MAX_PATH_UNITS));
    assert_eq!(
        encode_dropfiles(&[too_long]),
        Err(DropFilesError::PathTooLong)
    );
    let many: Vec<String> = (0..=MAX_ITEMS).map(|i| format!("C:\\f{i}")).collect();
    assert_eq!(encode_dropfiles(&many), Err(DropFilesError::TooManyItems));
}

#[test]
fn parser_refuses_truncated_header() {
    assert_eq!(parse_dropfiles(&[]), Err(DropFilesError::Truncated));
    assert_eq!(parse_dropfiles(&[0u8; 19]), Err(DropFilesError::Truncated));
}

#[test]
fn parser_refuses_offset_outside_block_or_inside_header() {
    let mut body = units(r"C:\a");
    body.extend([0, 0]);
    // pFiles beyond the block.
    assert_eq!(
        parse_dropfiles(&raw(10_000, 1, &body)),
        Err(DropFilesError::BadOffset)
    );
    assert_eq!(
        parse_dropfiles(&raw(u32::MAX, 1, &body)),
        Err(DropFilesError::BadOffset)
    );
    // pFiles pointing into the header.
    assert_eq!(
        parse_dropfiles(&raw(4, 1, &body)),
        Err(DropFilesError::BadOffset)
    );
    // pFiles exactly at the end: an empty body has no terminator.
    let empty = raw(20, 1, &[]);
    assert_eq!(parse_dropfiles(&empty), Err(DropFilesError::NotTerminated));
}

#[test]
fn parser_refuses_missing_terminators() {
    // No NUL at all.
    assert_eq!(
        parse_dropfiles(&raw(20, 1, &units(r"C:\a"))),
        Err(DropFilesError::NotTerminated)
    );
    // Path NUL but no closing empty string.
    let mut one = units(r"C:\a");
    one.push(0);
    assert_eq!(
        parse_dropfiles(&raw(20, 1, &one)),
        Err(DropFilesError::NotTerminated)
    );
}

#[test]
fn parser_refuses_odd_length_body() {
    let mut bytes = encode_dropfiles(&[s(r"C:\a")]).unwrap();
    bytes.push(0);
    assert_eq!(parse_dropfiles(&bytes), Err(DropFilesError::Unaligned));
}

#[test]
fn parser_refuses_ansi_lists() {
    let mut ansi = raw(20, 0, &[]);
    ansi.extend_from_slice(b"C:\\a\0\0");
    assert_eq!(parse_dropfiles(&ansi), Err(DropFilesError::Ansi));
}

#[test]
fn parser_refuses_empty_list() {
    assert_eq!(
        parse_dropfiles(&raw(20, 1, &[0, 0])),
        Err(DropFilesError::Empty)
    );
}

#[test]
fn parser_refuses_lone_surrogates() {
    let body = [0xD800u16, 0x41, 0, 0];
    assert_eq!(
        parse_dropfiles(&raw(20, 1, &body)),
        Err(DropFilesError::BadText)
    );
}

#[test]
fn parser_limits_items_and_sizes() {
    let mut body: Vec<u16> = Vec::new();
    for i in 0..=MAX_ITEMS {
        body.extend(units(&format!("C:\\f{i}")));
        body.push(0);
    }
    body.push(0);
    assert_eq!(
        parse_dropfiles(&raw(20, 1, &body)),
        Err(DropFilesError::TooManyItems)
    );

    let mut long = units("C:\\");
    long.extend(std::iter::repeat_n(0x64u16, MAX_PATH_UNITS));
    long.extend([0, 0]);
    assert_eq!(
        parse_dropfiles(&raw(20, 1, &long)),
        Err(DropFilesError::PathTooLong)
    );

    let oversize = vec![0u8; MAX_BLOCK_BYTES + 1];
    assert_eq!(parse_dropfiles(&oversize), Err(DropFilesError::TooMuchText));
}

#[test]
fn parser_stops_at_terminator_and_ignores_trailing_bytes() {
    let mut body = units(r"C:\a");
    body.extend([0, 0]);
    body.extend(units(r"C:\ignored"));
    body.extend([0, 0]);
    assert_eq!(
        parse_dropfiles(&raw(20, 1, &body)).unwrap(),
        vec![s(r"C:\a")]
    );
}

#[test]
fn parser_honours_p_files_beyond_the_header() {
    // pFiles = 24: four filler bytes between the header and the list.
    let mut bytes = raw(24, 1, &[]);
    bytes.extend_from_slice(&[0xAA; 4]);
    for u in units(r"C:\a").into_iter().chain([0, 0]) {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    assert_eq!(parse_dropfiles(&bytes).unwrap(), vec![s(r"C:\a")]);
}

#[test]
fn drop_effect_encoding_and_decoding() {
    assert_eq!(encode_drop_effect(false), 1u32.to_le_bytes());
    assert_eq!(encode_drop_effect(true), 2u32.to_le_bytes());
    assert!(effect_is_move(Some(&2u32.to_le_bytes())));
    assert!(!effect_is_move(Some(&1u32.to_le_bytes())));
    // Copy | link and copy | move are copies; link alone and none are copies.
    assert!(!effect_is_move(Some(&5u32.to_le_bytes())));
    assert!(!effect_is_move(Some(&3u32.to_le_bytes())));
    assert!(!effect_is_move(Some(&4u32.to_le_bytes())));
    assert!(!effect_is_move(Some(&0u32.to_le_bytes())));
    // Absent or short block: copy.
    assert!(!effect_is_move(None));
    assert!(!effect_is_move(Some(&[2, 0])));
}

/// Real clipboard round trip. The clipboard is machine-global and a CI session may have no
/// usable desktop clipboard (or another process may hold it), so this is opt-in:
/// `cargo test -- --ignored clipboard_roundtrip`. The parser tests above cover the logic.
#[cfg(windows)]
#[test]
#[ignore = "uses the real, machine-global Windows clipboard; not reliable on CI sessions"]
fn clipboard_roundtrip_with_real_clipboard() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow};
    let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
    // SAFETY: creates and destroys a hidden helper window on this thread; the class is a
    // system class and every pointer argument is null or a live NUL-terminated string.
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    assert!(!hwnd.is_null());
    let h = hwnd as isize;
    let set = vec![s(r"C:\Windows\notepad.exe"), s(r"C:\Windows\System32")];

    let seq = write_files(h, &set, true).unwrap();
    let got = read_files(h).unwrap().unwrap();
    assert_eq!(got.paths, set);
    assert!(got.cut);
    assert_eq!(got.sequence, seq);

    write_files(h, &set, false).unwrap();
    let got = read_files(h).unwrap().unwrap();
    assert!(!got.cut);

    // A stale sequence number leaves the clipboard alone; the current one clears it.
    assert!(!clear_if_unchanged(h, got.sequence.wrapping_add(1)).unwrap());
    assert!(read_files(h).unwrap().is_some());
    assert!(clear_if_unchanged(h, got.sequence).unwrap());
    assert!(read_files(h).unwrap().is_none());

    // SAFETY: `hwnd` was created above and is destroyed once.
    unsafe {
        DestroyWindow(hwnd);
    }
}

/// Manual interop check: put files on the clipboard from Explorer / PowerShell
/// (`Set-Clipboard -Path ...`), then run `cargo test -- --ignored reads_files_copied_elsewhere
/// --nocapture`. Prints what was read.
#[cfg(windows)]
#[test]
#[ignore = "manual interop check against the real clipboard"]
fn reads_files_copied_elsewhere() {
    let got = read_files(0).unwrap();
    println!("{got:?}");
    assert!(got.is_some());
}

/// Manual interop check, the other direction: leaves a CUT file list on the clipboard so
/// Explorer (or `Get-Clipboard -Format FileDropList`) can paste it.
#[cfg(windows)]
#[test]
#[ignore = "manual interop check; replaces the real clipboard"]
fn leaves_cut_files_for_explorer() {
    use windows_sys::Win32::UI::WindowsAndMessaging::CreateWindowExW;
    let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
    // SAFETY: hidden helper window kept for the process lifetime; arguments are null or live.
    let hwnd = unsafe {
        CreateWindowExW(
            0,
            class.as_ptr(),
            class.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null(),
        )
    };
    let paths = vec![s(r"C:\Windows\notepad.exe")];
    write_files(hwnd as isize, &paths, true).unwrap();
}
