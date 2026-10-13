//! Total Commander `[DirMenu]` import (FER-458). Read-only: the ini is decoded and parsed into a
//! v2 hotlist for preview; nothing here writes, and no `command` item is ever launched.
//!
//! Rules (verified on a real wincmd.ini, see `contract-fixtures/hotlist/`):
//! - `menuN=-Title` starts a submenu, `menuN=--` ends it, `menuN=-` is a separator.
//! - `cmdN=cd <path>` is a folder; any other `cmdN` is a command (program + args).
//! - `menuN` without `cmdN` has no action and is skipped with a warning; `cm_*`/`em_*` internal
//!   TC commands are skipped too (Ferrum cannot run them).
//! - `pathN` becomes `targetPath` (opposite pane).
//! - Titles stay verbatim (numbers and `&` accelerators included); empty submenus are kept.
//! - Numbering stops at the first missing `menuN`, exactly like TC.

use std::collections::BTreeMap;

use serde::Serialize;

use super::model::{HotlistDoc, Item, MAX_DEPTH, MAX_ITEMS, SCHEMA_VERSION};

/// A wincmd.ini bigger than this is not something we parse.
pub const MAX_INI_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportWarning {
    /// `menuN` index the warning is about (0 = whole section).
    pub index: u32,
    /// no_action | internal_command | unbalanced_end | unclosed_submenu | ignored_after_gap |
    /// too_deep | truncated | invalid_entry | device_path (FER-467 H1)
    pub kind: &'static str,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    pub doc: HotlistDoc,
    pub warnings: Vec<ImportWarning>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportError {
    /// The file has no `[DirMenu]` section (or it is empty).
    NoDirMenu,
}

/// Decode a wincmd.ini: UTF-16LE/BE (TC writes these with a BOM), UTF-8 (with or without BOM),
/// otherwise Windows-1252.
pub fn decode_ini(bytes: &[u8]) -> String {
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = rest
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        Err(_) => bytes.iter().map(|&b| cp1252(b)).collect(),
    }
}

fn cp1252(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž',
        '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}',
        'ž', 'Ÿ',
    ];
    match b {
        0x80..=0x9F => HIGH[(b - 0x80) as usize],
        _ => b as char,
    }
}

#[derive(Default)]
struct Entry {
    menu: Option<String>,
    cmd: Option<String>,
    path: Option<String>,
}

/// Collect `menuN` / `cmdN` / `pathN` of the `[DirMenu]` section, keyed by N.
fn dirmenu_entries(text: &str) -> Option<BTreeMap<u32, Entry>> {
    let mut in_section = false;
    let mut found = false;
    let mut entries: BTreeMap<u32, Entry> = BTreeMap::new();
    for raw in text.lines() {
        let line = raw.trim_end_matches('\r');
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_section = trimmed.eq_ignore_ascii_case("[DirMenu]");
            found |= in_section;
            continue;
        }
        if !in_section || trimmed.starts_with(';') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let (field, digits) = if let Some(d) = key.strip_prefix("menu") {
            (0, d)
        } else if let Some(d) = key.strip_prefix("cmd") {
            (1, d)
        } else if let Some(d) = key.strip_prefix("path") {
            (2, d)
        } else {
            continue;
        };
        let Ok(n) = digits.parse::<u32>() else {
            continue;
        };
        let slot = entries.entry(n).or_default();
        let value = Some(value.to_string());
        match field {
            0 => slot.menu = value,
            1 => slot.cmd = value,
            _ => slot.path = value,
        }
    }
    found.then_some(entries)
}

/// `c:\x` → `C:\x`; surrounding quotes dropped. Anything else verbatim.
fn normalize_path(raw: &str) -> String {
    let s = raw.trim();
    let s = s
        .strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .unwrap_or(s);
    let mut chars = s.chars();
    match (chars.next(), chars.next()) {
        (Some(d), Some(':')) if d.is_ascii_alphabetic() => {
            format!("{}{}", d.to_ascii_uppercase(), &s[1..])
        }
        _ => s.to_string(),
    }
}

/// Split argument text the way CommandLineToArgvW does for simple cases: whitespace separates,
/// double quotes group (and are removed).
fn split_args(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut has = false;
    for c in s.chars() {
        match c {
            '"' => {
                quoted = !quoted;
                has = true;
            }
            c if c.is_whitespace() && !quoted => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                cur.push(c);
                has = true;
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}

const PROGRAM_EXTS: [&str; 9] = [
    ".exe", ".com", ".bat", ".cmd", ".lnk", ".ahk", ".ps1", ".vbs", ".msc",
];

/// `"C:\a b\x.exe" -y` / `C:\a b\x.exe -y` / `C:\a b\forward to kando.exe` → (program, args).
/// Unquoted programs end at the first known program extension followed by whitespace or the
/// end, so spaces inside the path survive.
pub fn split_command(raw: &str) -> (String, Vec<String>) {
    let s = raw.trim();
    if let Some(rest) = s.strip_prefix('"')
        && let Some(end) = rest.find('"')
    {
        return (normalize_path(&rest[..end]), split_args(&rest[end + 1..]));
    }
    let lower = s.to_ascii_lowercase();
    let mut best: Option<usize> = None;
    for ext in PROGRAM_EXTS {
        let mut from = 0;
        while let Some(pos) = lower[from..].find(ext) {
            let end = from + pos + ext.len();
            if end == s.len() || s[end..].starts_with(char::is_whitespace) {
                best = Some(best.map_or(end, |b: usize| b.min(end)));
                break;
            }
            from = end;
        }
    }
    match best {
        Some(end) => (normalize_path(&s[..end]), split_args(&s[end..])),
        None => (normalize_path(s), Vec::new()),
    }
}

/// One open submenu while parsing. A submenu whose title is not valid is dropped with its content.
struct Frame {
    title: String,
    items: Vec<Item>,
    keep: bool,
}

fn frame(title: &str, keep: bool) -> Frame {
    Frame {
        title: title.to_string(),
        items: Vec::new(),
        keep,
    }
}

/// Close the top frame into its parent (dropped when its own title was invalid).
fn close_top(stack: &mut Vec<Frame>) {
    let Some(done) = stack.pop() else { return };
    if done.keep
        && let Some(parent) = stack.last_mut()
    {
        parent.items.push(Item::Submenu {
            title: done.title,
            items: done.items,
        });
    }
}

fn submenu_title_ok(title: &str) -> bool {
    HotlistDoc {
        version: SCHEMA_VERSION,
        source: None,
        items: vec![Item::Submenu {
            title: title.to_string(),
            items: Vec::new(),
        }],
    }
    .validate()
    .is_ok()
}

/// The three-byte prefix of `cmd` when it is one (char boundary safe: `a€x` has none).
fn prefix3(cmd: &str) -> Option<&str> {
    cmd.get(..3)
}

/// Parse decoded wincmd.ini text into a v2 hotlist.
pub fn parse_dirmenu(text: &str) -> Result<ImportResult, ImportError> {
    let entries = dirmenu_entries(text).ok_or(ImportError::NoDirMenu)?;
    let mut warnings = Vec::new();
    // Open submenus; index 0 is the root.
    let mut stack: Vec<Frame> = vec![frame("", true)];
    let mut count = 0usize;
    let mut expected = 1u32;
    for (&n, entry) in &entries {
        if entry.menu.is_none() {
            continue;
        }
        if n != expected {
            warnings.push(ImportWarning {
                index: n,
                kind: "ignored_after_gap",
            });
            break;
        }
        expected += 1;
        if count >= MAX_ITEMS {
            warnings.push(ImportWarning {
                index: n,
                kind: "truncated",
            });
            break;
        }
        let menu = entry.menu.as_deref().unwrap_or_default().trim();
        if menu == "--" {
            if stack.len() > 1 {
                close_top(&mut stack);
            } else {
                warnings.push(ImportWarning {
                    index: n,
                    kind: "unbalanced_end",
                });
            }
            continue;
        }
        if menu == "-" {
            count += 1;
            if let Some(top) = stack.last_mut() {
                top.items.push(Item::Separator);
            }
            continue;
        }
        if let Some(title) = menu.strip_prefix('-') {
            if stack.len() > MAX_DEPTH {
                warnings.push(ImportWarning {
                    index: n,
                    kind: "too_deep",
                });
                return Ok(finish(stack, warnings));
            }
            count += 1;
            let keep = submenu_title_ok(title);
            if !keep {
                warnings.push(ImportWarning {
                    index: n,
                    kind: "invalid_entry",
                });
            }
            stack.push(frame(title, keep));
            continue;
        }
        let cmd = entry.cmd.as_deref().map(str::trim).unwrap_or_default();
        let prefix = prefix3(cmd);
        let item = if cmd.is_empty() {
            warnings.push(ImportWarning {
                index: n,
                kind: "no_action",
            });
            None
        } else if prefix.is_some_and(|p| p.eq_ignore_ascii_case("cd "))
            && cmd.len() > 3
            && (super::model::is_device_path(&normalize_path(&cmd[3..]))
                || entry
                    .path
                    .as_deref()
                    .is_some_and(|p| super::model::is_device_path(&normalize_path(p))))
        {
            // FER-467 H1: device / NT namespace folders are refused with their own reason.
            warnings.push(ImportWarning {
                index: n,
                kind: "device_path",
            });
            None
        } else if prefix.is_some_and(|p| p.eq_ignore_ascii_case("cd ")) && cmd.len() > 3 {
            Some(Item::Dir {
                title: menu.to_string(),
                path: normalize_path(&cmd[3..]),
                target_path: entry
                    .path
                    .as_deref()
                    .map(normalize_path)
                    .filter(|p| !p.is_empty()),
            })
        } else if prefix
            .is_some_and(|p| p.eq_ignore_ascii_case("cm_") || p.eq_ignore_ascii_case("em_"))
            && cmd.len() > 3
        {
            warnings.push(ImportWarning {
                index: n,
                kind: "internal_command",
            });
            None
        } else {
            let (command, args) = split_command(cmd);
            Some(Item::Command {
                title: menu.to_string(),
                command,
                args,
            })
        };
        if let Some(item) = item {
            // Each item is validated alone so one odd entry never sinks the whole import.
            let probe = HotlistDoc {
                version: SCHEMA_VERSION,
                source: None,
                items: vec![item],
            };
            if probe.validate().is_ok() {
                count += 1;
                if let (Some(top), Some(item)) = (stack.last_mut(), probe.items.into_iter().next())
                {
                    top.items.push(item);
                }
            } else {
                warnings.push(ImportWarning {
                    index: n,
                    kind: "invalid_entry",
                });
            }
        }
    }
    if stack.len() > 1 {
        warnings.push(ImportWarning {
            index: 0,
            kind: "unclosed_submenu",
        });
    }
    Ok(finish(stack, warnings))
}

/// Close any submenus left open and wrap the root.
fn finish(mut stack: Vec<Frame>, warnings: Vec<ImportWarning>) -> ImportResult {
    while stack.len() > 1 {
        close_top(&mut stack);
    }
    let items = stack.pop().map(|f| f.items).unwrap_or_default();
    ImportResult {
        doc: HotlistDoc {
            version: SCHEMA_VERSION,
            source: Some("Total Commander [DirMenu]".into()),
            items,
        },
        warnings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE_INI: &str = include_str!("../../../contract-fixtures/hotlist/tc-dirmenu.ini");
    const FIXTURE_JSON: &str =
        include_str!("../../../contract-fixtures/hotlist/tc-dirmenu.expected.json");

    fn parse(text: &str) -> ImportResult {
        parse_dirmenu(text).expect("dirmenu")
    }

    #[test]
    fn contract_fixture_converts_exactly_to_the_expected_tree() {
        let expected = HotlistDoc::parse(FIXTURE_JSON.as_bytes()).expect("fixture is valid v2");
        let result = parse(FIXTURE_INI);
        assert_eq!(result.warnings, vec![]);
        assert_eq!(result.doc.items, expected.items);
        assert!(result.doc.validate().is_ok());
    }

    #[test]
    fn contract_fixture_survives_utf16_and_crlf_encodings() {
        let crlf = FIXTURE_INI.replace('\n', "\r\n");
        let mut utf16 = vec![0xFF, 0xFE];
        for unit in crlf.encode_utf16() {
            utf16.extend_from_slice(&unit.to_le_bytes());
        }
        let expected = parse(FIXTURE_INI).doc;
        assert_eq!(parse(&decode_ini(&utf16)).doc, expected);
        let mut bom8 = vec![0xEF, 0xBB, 0xBF];
        bom8.extend_from_slice(crlf.as_bytes());
        assert_eq!(parse(&decode_ini(&bom8)).doc, expected);
    }

    #[test]
    fn decodes_ansi_ini_as_windows_1252() {
        let bytes = b"[DirMenu]\nmenu1=Caf\xe9 \x80\ncmd1=cd C:\\x\n";
        let doc = parse(&decode_ini(bytes)).doc;
        assert_eq!(
            doc.items[0],
            Item::Dir {
                title: "Café €".into(),
                path: "C:\\x".into(),
                target_path: None
            }
        );
    }

    #[test]
    fn single_dash_is_a_separator_and_double_dash_closes_a_submenu() {
        let ini = "[DirMenu]\nmenu1=-Sub\nmenu2=A\ncmd2=cd c:\\a\nmenu3=-\nmenu4=--\nmenu5=-\nmenu6=B\ncmd6=cd d:\\b\n";
        let r = parse(ini);
        assert_eq!(
            r.doc.items,
            vec![
                Item::Submenu {
                    title: "Sub".into(),
                    items: vec![
                        Item::Dir {
                            title: "A".into(),
                            path: "C:\\a".into(),
                            target_path: None
                        },
                        Item::Separator,
                    ]
                },
                Item::Separator,
                Item::Dir {
                    title: "B".into(),
                    path: "D:\\b".into(),
                    target_path: None
                },
            ]
        );
        assert!(r.warnings.is_empty());
    }

    #[test]
    fn maps_target_path_commands_and_warns_on_skipped_entries() {
        let ini = "[dirmenu]\r\nmenu1=Both\r\ncmd1=CD c:\\left\r\npath1=d:\\right\r\nmenu2=Tool\r\ncmd2=\"C:\\Program Files\\t.exe\" -x \"a b\"\r\nmenu3=Nothing\r\nmenu4=Internal\r\ncmd4=cm_DirMatch\r\nmenu5=--\r\n";
        let r = parse(ini);
        assert_eq!(
            r.doc.items,
            vec![
                Item::Dir {
                    title: "Both".into(),
                    path: "C:\\left".into(),
                    target_path: Some("D:\\right".into())
                },
                Item::Command {
                    title: "Tool".into(),
                    command: "C:\\Program Files\\t.exe".into(),
                    args: vec!["-x".into(), "a b".into()]
                },
            ]
        );
        let kinds: Vec<_> = r.warnings.iter().map(|w| (w.index, w.kind)).collect();
        assert_eq!(
            kinds,
            vec![
                (3, "no_action"),
                (4, "internal_command"),
                (5, "unbalanced_end")
            ]
        );
    }

    #[test]
    fn stops_at_the_first_gap_like_total_commander() {
        let ini = "[DirMenu]\nmenu1=A\ncmd1=cd C:\\a\nmenu3=C\ncmd3=cd C:\\c\n";
        let r = parse(ini);
        assert_eq!(r.doc.items.len(), 1);
        assert_eq!(
            r.warnings,
            vec![ImportWarning {
                index: 3,
                kind: "ignored_after_gap"
            }]
        );
    }

    #[test]
    fn closes_unclosed_submenus_and_keeps_empty_ones() {
        let r = parse("[DirMenu]\nmenu1=-Empty\nmenu2=--\nmenu3=-Open\nmenu4=X\ncmd4=cd C:\\x\n");
        assert_eq!(r.doc.items.len(), 2);
        assert_eq!(
            r.doc.items[0],
            Item::Submenu {
                title: "Empty".into(),
                items: vec![]
            }
        );
        assert!(matches!(&r.doc.items[1], Item::Submenu { items, .. } if items.len() == 1));
        assert_eq!(r.warnings[0].kind, "unclosed_submenu");
    }

    #[test]
    fn refuses_files_without_a_dirmenu_section_and_ignores_other_sections() {
        assert_eq!(
            parse_dirmenu("[Configuration]\nmenu1=x\n"),
            Err(ImportError::NoDirMenu)
        );
        let r = parse(
            "[Other]\nmenu1=Z\ncmd1=cd C:\\z\n[DirMenu]\nmenu1=A\ncmd1=cd C:\\a\n[Next]\nmenu2=B\ncmd2=cd C:\\b\n",
        );
        assert_eq!(r.doc.items.len(), 1);
    }

    #[test]
    fn multibyte_command_prefixes_never_panic() {
        // 0x80 is "€" in ANSI; "a€x" has no char boundary at byte 3.
        for cmd in ["a€x", "€€", "c€", "cd€", "ab€d", "é", "日本語のコマンド"]
        {
            let ini = format!("[DirMenu]\nmenu1=T\ncmd1={cmd}\n");
            let r = parse(&ini);
            assert!(r.doc.validate().is_ok(), "{cmd}");
        }
        let r = parse("[DirMenu]\nmenu1=T\ncmd1=a€x\n");
        assert!(matches!(&r.doc.items[0], Item::Command { command, .. } if command == "a€x"));
    }

    #[test]
    fn invalid_submenu_titles_are_dropped_with_their_content_and_a_warning() {
        let long = "x".repeat(300);
        let ini = format!(
            "[DirMenu]\nmenu1=-{long}\nmenu2=Lost\ncmd2=cd C:\\lost\nmenu3=--\nmenu4=-evil\u{202E}\nmenu5=Lost2\ncmd5=cd C:\\lost2\nmenu6=--\nmenu7=Kept\ncmd7=cd C:\\kept\n"
        );
        let r = parse(&ini);
        assert_eq!(r.doc.items.len(), 1);
        assert!(matches!(&r.doc.items[0], Item::Dir { title, .. } if title == "Kept"));
        let invalid = r
            .warnings
            .iter()
            .filter(|w| w.kind == "invalid_entry")
            .count();
        assert_eq!(invalid, 2);
        assert!(r.doc.validate().is_ok());
    }

    #[test]
    fn unsafe_commands_are_skipped_instead_of_failing_the_whole_import() {
        let ini = "[DirMenu]\n\
            menu1=Batch\ncmd1=C:\\s\\run.bat \"x & calc & \"\n\
            menu2=Uri\ncmd2=ms-msdt:/id Foo\n\
            menu3=Unc\ncmd3=\\\\host@SSL\\share\\x.exe\n\
            menu4=Fine\ncmd4=C:\\ok\\tool.exe -a\n";
        let r = parse(ini);
        assert_eq!(r.doc.items.len(), 1);
        assert!(matches!(&r.doc.items[0], Item::Command { title, .. } if title == "Fine"));
        assert_eq!(
            r.warnings.iter().map(|w| w.index).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(r.warnings.iter().all(|w| w.kind == "invalid_entry"));
    }

    #[test]
    fn rejects_nesting_beyond_the_depth_limit_without_panicking() {
        let mut ini = String::from("[DirMenu]\n");
        for i in 1..=(MAX_DEPTH as u32 + 3) {
            ini.push_str(&format!("menu{i}=-s{i}\n"));
        }
        let r = parse(&ini);
        assert!(r.warnings.iter().any(|w| w.kind == "too_deep"));
        assert!(r.doc.validate().is_ok());
    }

    #[test]
    fn splits_unquoted_programs_with_spaces_at_the_extension() {
        assert_eq!(
            split_command("c:\\a b\\forward to kando.exe"),
            ("C:\\a b\\forward to kando.exe".into(), vec![])
        );
        assert_eq!(
            split_command("C:\\a b\\x.exe -q \"two words\""),
            (
                "C:\\a b\\x.exe".into(),
                vec!["-q".into(), "two words".into()]
            )
        );
        assert_eq!(
            split_command("notepad.exe C:\\n.txt"),
            ("notepad.exe".into(), vec!["C:\\n.txt".into()])
        );
        assert_eq!(
            split_command("C:\\tool.exe.bak\\run.cmd x"),
            ("C:\\tool.exe.bak\\run.cmd".into(), vec!["x".into()])
        );
        assert_eq!(
            split_command("https://example.com"),
            ("https://example.com".into(), vec![])
        );
    }

    /// FER-467 H1: device spellings of a folder (some reach a network host) are skipped with
    /// their own warning; ordinary folders around them still import.
    #[test]
    fn device_path_folders_are_skipped_with_a_reason() {
        let ini = "[DirMenu]\n\
            menu1=Mup\ncmd1=cd \\\\?\\GLOBALROOT\\Device\\Mup\\evil\\share\n\
            menu2=Dot UNC\ncmd2=cd \\\\.\\UNC\\evil\\share\n\
            menu3=Target\ncmd3=cd C:\\ok\npath3=\\??\\UNC\\evil\\x\n\
            menu4=Fine\ncmd4=cd C:\\fine\n";
        let r = parse(ini);
        assert_eq!(
            r.warnings.iter().map(|w| w.kind).collect::<Vec<_>>(),
            vec!["device_path"; 3]
        );
        assert_eq!(r.doc.items.len(), 1);
        assert!(r.doc.validate().is_ok());
    }
}
