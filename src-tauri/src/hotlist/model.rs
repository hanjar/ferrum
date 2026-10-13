//! Directory Hotlist schema v2 (FER-458): a tree of `dir` / `submenu` / `separator` / `command`
//! items. Validated on load, on save and on import; anything outside the limits is rejected as a
//! whole so a hand-edited or synced file can never half-apply.

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 2;
/// Submenu nesting below the top level (TC users rarely go past 3).
pub const MAX_DEPTH: usize = 8;
/// Every item in the tree, separators included.
pub const MAX_ITEMS: usize = 2000;
pub const MAX_TITLE_CHARS: usize = 256;
pub const MAX_PATH_CHARS: usize = 4096;
pub const MAX_ARGS: usize = 32;
/// Upper bound for the serialized file; a bigger file is not a hotlist.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Item {
    Dir {
        /// Display title; `&` marks the accelerator, kept verbatim.
        title: String,
        path: String,
        /// TC `pathN`: folder for the opposite pane.
        #[serde(
            rename = "targetPath",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        target_path: Option<String>,
    },
    Submenu {
        title: String,
        #[serde(default)]
        items: Vec<Item>,
    },
    Separator,
    Command {
        title: String,
        /// Program or document; launched with ShellExecute, never through a shell string.
        command: String,
        #[serde(default)]
        args: Vec<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HotlistDoc {
    pub version: u32,
    /// Free-form provenance note (e.g. "Total Commander [DirMenu]"); never interpreted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default)]
    pub items: Vec<Item>,
}

impl Default for HotlistDoc {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            source: None,
            items: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidReason {
    Version,
    TooDeep,
    TooMany,
    Title,
    Path,
    Command,
    Args,
}

/// Unicode "format" characters (category Cf) that can reorder or hide text: bidi controls
/// (U+202A–202E, U+2066–2069, U+200E/F, U+061C), zero-width and invisible operators, BOM, tag
/// characters and so on. ZWNJ/ZWJ (U+200C/D) stay allowed: some scripts need them.
pub fn is_format_char(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x0600..=0x0605 | 0x061C | 0x06DD | 0x070F | 0x0890..=0x0891 | 0x08E2 | 0x180E
        | 0x200B | 0x200E | 0x200F | 0x202A..=0x202E | 0x2060..=0x2064 | 0x2066..=0x206F
        | 0xFEFF | 0xFFF9..=0xFFFB | 0x110BD | 0x110CD | 0x13430..=0x1343F | 0x1BCA0..=0x1BCA3
        | 0x1D173..=0x1D17A | 0xE0001 | 0xE0020..=0xE007F)
}

fn clean_text(s: &str, max: usize) -> bool {
    s.chars().count() <= max && !s.chars().any(|c| c.is_control() || is_format_char(c))
}

fn valid_title(s: &str) -> bool {
    !s.trim().is_empty() && clean_text(s, MAX_TITLE_CHARS)
}

fn valid_path(s: &str) -> bool {
    crate::is_plausible_path(s) && clean_text(s, MAX_PATH_CHARS)
}

/// FER-467 H1: device and NT namespace spellings of a folder, in any separator mix: `\\.\…`,
/// `\\?\…` (except the local long form `\\?\X:\…`, which must then pass `path_guard::check_str`),
/// `\??\…`, `GLOBALROOT`, `\Device\…` (e.g. `\Device\Mup\host\share`). Several of them reach a
/// network host (`\\.\UNC\host`, `\\?\GLOBALROOT\Device\Mup\host`) without looking like UNC, so a
/// synced hotlist could hide a network folder behind them; none is a folder a user means.
pub fn is_device_path(s: &str) -> bool {
    let p = s.trim().replace('/', "\\");
    let lower = p.to_ascii_lowercase();
    // `\??\…` (NT object path), `\Device\…`, and `\Device\Mup\` (the network redirector)
    // anywhere. `\\?\GLOBALROOT` and `\\.\GLOBALROOT` are caught by the prefix rules below.
    if lower.starts_with("\\??\\")
        || lower.starts_with("\\device\\")
        || lower.contains("\\device\\mup\\")
    {
        return true;
    }
    if let Some(rest) = p.strip_prefix("\\\\?\\") {
        let b = rest.as_bytes();
        let local = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && b[2] == b'\\';
        return !local || crate::path_guard::check_str(rest).is_err();
    }
    let b = p.as_bytes();
    // `\\.\…`, and the bare `\\.` / `\\?` roots.
    b.len() >= 3
        && b[0] == b'\\'
        && b[1] == b'\\'
        && matches!(b[2], b'.' | b'?')
        && (b.len() == 3 || b[3] == b'\\')
}

/// A folder of a `dir` item (path or target path): a plausible path that is not a device path.
fn valid_dir_path(s: &str) -> bool {
    valid_path(s) && !is_device_path(s)
}

/// `scheme:` with two or more characters before the colon (`https:`, `ms-msdt:`, `search-ms:`);
/// a lone letter is a drive (`C:`).
fn has_uri_scheme(s: &str) -> bool {
    let Some((scheme, _)) = s.split_once(':') else {
        return false;
    };
    let mut chars = scheme.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && scheme.chars().count() >= 2
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '.' | '-'))
}

fn is_sep(c: char) -> bool {
    c == '\\' || c == '/'
}

/// A command is a local program path or a bare program name. Refused: URI schemes, anything that
/// starts with two separators in any mix (`\\`, `//`, `\/`, `/\`: UNC/WebDAV/device, which make
/// Windows contact a host) or with the NT prefix `\??\`; the characters `" < > | * ?` (none can be
/// part of a program path, and `"` would break the shown command line); and a `:` anywhere except
/// right after a drive letter (alternate data streams: `x.exe:stream`, `x.exe::$DATA`).
/// Shared vectors: `contract-fixtures/hotlist/command-cases.json`.
fn valid_command(s: &str) -> bool {
    let t = s.trim();
    let mut chars = t.chars();
    let two_separators =
        matches!((chars.next(), chars.next()), (Some(a), Some(b)) if is_sep(a) && is_sep(b));
    let colon_ok = t
        .match_indices(':')
        .all(|(i, _)| i == 1 && t.chars().next().is_some_and(|c| c.is_ascii_alphabetic()));
    valid_path(s)
        && !has_uri_scheme(t)
        && !two_separators
        && !t.starts_with("\\??\\")
        && !s.contains(['"', '<', '>', '|', '*', '?'])
        && colon_ok
}

/// cmd.exe interprets these in arguments of a batch file (CVE-2024-24576 "BatBadBut").
const CMD_META: [char; 7] = ['"', '%', '&', '|', '<', '>', '^'];

/// True unless `command` is a plain `.exe`/`.com` (trailing dots and spaces are ignored by
/// Windows). Everything else may end up in cmd.exe: `.bat`/`.cmd`, shortcuts that point at one,
/// names with another or no extension (PATHEXT may resolve them), so arguments get the strict check.
pub fn needs_cmd_safe_args(command: &str) -> bool {
    let name = command
        .trim()
        .trim_end_matches(['.', ' '])
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    !matches!(name.rsplit_once('.'), Some((_, "exe" | "com")))
}

/// Arguments that must not reach a program that may run through cmd.exe: cmd metacharacters or
/// line breaks.
pub fn batch_unsafe_args(command: &str, args: &[String]) -> bool {
    needs_cmd_safe_args(command)
        && args.iter().any(|a| {
            a.chars()
                .any(|c| CMD_META.contains(&c) || c == '\n' || c == '\r')
        })
}

/// What kind of program a command starts, for the warning in the confirmation (mirrors
/// `commandRisk` in `src/lib/hotlist/model.ts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Risk {
    Script,
    Shortcut,
}

pub fn command_risk(command: &str) -> Option<Risk> {
    let t = command.trim().trim_end_matches(['.', ' ']);
    let name = t.rsplit(['\\', '/']).next().unwrap_or_default();
    let (_, ext) = name.rsplit_once('.')?;
    match ext.to_ascii_lowercase().as_str() {
        "lnk" | "url" => Some(Risk::Shortcut),
        "ps1" | "vbs" | "vbe" | "js" | "jse" | "wsf" | "wsh" | "hta" | "scr" | "pif" | "msc"
        | "reg" | "bat" | "cmd" => Some(Risk::Script),
        _ => None,
    }
}

/// Title without the `&` accelerator markers (`&&` stays a literal `&`).
pub fn plain_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut chars = title.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '&' {
            if chars.peek() == Some(&'&') {
                chars.next();
                out.push('&');
            }
            continue;
        }
        out.push(c);
    }
    out
}

fn validate_items(items: &[Item], depth: usize, count: &mut usize) -> Result<(), InvalidReason> {
    for item in items {
        *count += 1;
        if *count > MAX_ITEMS {
            return Err(InvalidReason::TooMany);
        }
        match item {
            Item::Dir {
                title,
                path,
                target_path,
            } => {
                if !valid_title(title) {
                    return Err(InvalidReason::Title);
                }
                if !valid_dir_path(path)
                    || target_path.as_deref().is_some_and(|t| !valid_dir_path(t))
                {
                    return Err(InvalidReason::Path);
                }
            }
            Item::Submenu { title, items } => {
                if !valid_title(title) {
                    return Err(InvalidReason::Title);
                }
                if depth >= MAX_DEPTH {
                    return Err(InvalidReason::TooDeep);
                }
                validate_items(items, depth + 1, count)?;
            }
            Item::Separator => {}
            Item::Command {
                title,
                command,
                args,
            } => {
                if !valid_title(title) {
                    return Err(InvalidReason::Title);
                }
                if !valid_command(command) {
                    return Err(InvalidReason::Command);
                }
                if args.len() > MAX_ARGS
                    || args
                        .iter()
                        .any(|a| a.contains('\0') || !clean_text(a, MAX_PATH_CHARS))
                {
                    return Err(InvalidReason::Args);
                }
                if batch_unsafe_args(command, args) {
                    return Err(InvalidReason::Args);
                }
            }
        }
    }
    Ok(())
}

impl HotlistDoc {
    pub fn validate(&self) -> Result<(), InvalidReason> {
        if self.version != SCHEMA_VERSION {
            return Err(InvalidReason::Version);
        }
        if self
            .source
            .as_deref()
            .is_some_and(|s| !clean_text(s, MAX_PATH_CHARS))
        {
            return Err(InvalidReason::Title);
        }
        let mut count = 0;
        validate_items(&self.items, 0, &mut count)
    }

    /// Parse + validate. `None` for anything that is not a valid v2 document.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        if bytes.len() as u64 > MAX_FILE_BYTES {
            return None;
        }
        let text = std::str::from_utf8(bytes).ok()?;
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let doc: HotlistDoc = serde_json::from_str(text).ok()?;
        doc.validate().ok()?;
        Some(doc)
    }

    /// The item at an index path (`[2, 0]` = first child of the third top-level item) and the
    /// titles of the submenus on the way to it, outermost first.
    pub fn item_at(&self, position: &[usize]) -> Option<(&Item, Vec<&str>)> {
        let (last, parents) = position.split_last()?;
        let mut items = &self.items;
        let mut trail = Vec::with_capacity(parents.len());
        for &i in parents {
            match items.get(i)? {
                Item::Submenu { title, items: kids } => {
                    trail.push(title.as_str());
                    items = kids;
                }
                _ => return None,
            }
        }
        Some((items.get(*last)?, trail))
    }

    /// True when an identical command item (program + args) exists anywhere in the tree.
    pub fn contains_command(&self, command: &str, args: &[String]) -> bool {
        fn walk(items: &[Item], command: &str, args: &[String]) -> bool {
            items.iter().any(|item| match item {
                Item::Command {
                    command: c,
                    args: a,
                    ..
                } => c == command && a.as_slice() == args,
                Item::Submenu { items, .. } => walk(items, command, args),
                _ => false,
            })
        }
        walk(&self.items, command, args)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(title: &str, path: &str) -> Item {
        Item::Dir {
            title: title.into(),
            path: path.into(),
            target_path: None,
        }
    }

    fn nest(depth: usize) -> Item {
        let mut item = dir("leaf", "C:\\");
        for _ in 0..depth {
            item = Item::Submenu {
                title: "s".into(),
                items: vec![item],
            };
        }
        item
    }

    #[test]
    fn round_trips_every_item_type_with_the_wire_names() {
        let doc = HotlistDoc {
            version: 2,
            source: None,
            items: vec![
                Item::Dir {
                    title: "&A".into(),
                    path: "C:\\a".into(),
                    target_path: Some("D:\\b".into()),
                },
                Item::Separator,
                Item::Submenu {
                    title: "S".into(),
                    items: vec![],
                },
                Item::Command {
                    title: "run".into(),
                    command: "C:\\x y\\z.exe".into(),
                    args: vec!["--flag".into(), "a b".into()],
                },
            ],
        };
        let text = serde_json::to_string(&doc).unwrap();
        assert!(text.contains(r#""type":"dir""#));
        assert!(text.contains(r#""targetPath":"D:\\b""#));
        assert!(text.contains(r#""type":"separator""#));
        assert_eq!(HotlistDoc::parse(text.as_bytes()), Some(doc));
    }

    #[test]
    fn rejects_wrong_version_garbage_and_oversized_input() {
        assert_eq!(HotlistDoc::parse(br#"{"version":1,"items":[]}"#), None);
        assert_eq!(HotlistDoc::parse(b"{nope"), None);
        assert_eq!(HotlistDoc::parse(b""), None);
        assert_eq!(
            HotlistDoc::parse(br#"{"version":2,"items":[{"type":"bogus"}]}"#),
            None
        );
        let big = vec![b' '; (MAX_FILE_BYTES + 1) as usize];
        assert_eq!(HotlistDoc::parse(&big), None);
        assert!(HotlistDoc::parse("\u{feff}{\"version\":2,\"items\":[]}".as_bytes()).is_some());
    }

    fn cmd(command: &str, args: &[&str]) -> Item {
        Item::Command {
            title: "c".into(),
            command: command.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
        }
    }

    fn check(item: Item) -> Result<(), InvalidReason> {
        HotlistDoc {
            items: vec![item],
            ..Default::default()
        }
        .validate()
    }

    #[test]
    fn rejects_bidi_and_other_format_characters_in_every_text_field() {
        for bad in [
            "\u{202E}",
            "\u{202A}",
            "\u{202D}",
            "\u{2066}",
            "\u{2069}",
            "\u{200E}",
            "\u{200F}",
            "\u{061C}",
            "\u{200B}",
            "\u{FEFF}",
            "\u{2060}",
            "\u{00AD}",
            "\u{E0041}",
        ] {
            let evil = format!("C:\\inv\\{bad}fdp.exe");
            assert_eq!(
                check(dir(&evil, "C:\\a")),
                Err(InvalidReason::Title),
                "{bad:?}"
            );
            assert_eq!(check(dir("a", &evil)), Err(InvalidReason::Path), "{bad:?}");
            assert_eq!(
                check(Item::Dir {
                    title: "a".into(),
                    path: "C:\\a".into(),
                    target_path: Some(evil.clone())
                }),
                Err(InvalidReason::Path)
            );
            assert_eq!(
                check(cmd(&evil, &[])),
                Err(InvalidReason::Command),
                "{bad:?}"
            );
            assert_eq!(
                check(cmd("C:\\a.exe", &[&evil])),
                Err(InvalidReason::Args),
                "{bad:?}"
            );
            assert_eq!(
                check(Item::Submenu {
                    title: evil.clone(),
                    items: vec![]
                }),
                Err(InvalidReason::Title)
            );
        }
        let with_source = HotlistDoc {
            source: Some("note\u{202E}".into()),
            ..Default::default()
        };
        assert!(with_source.validate().is_err());
        // ZWNJ/ZWJ are legitimate in some scripts and stay allowed.
        assert_eq!(check(dir("می\u{200C}خواهم", "C:\\a")), Ok(()));
    }

    #[test]
    fn batch_files_refuse_cmd_metacharacters_in_arguments() {
        for command in [
            "C:\\s\\run.bat",
            "C:\\s\\RUN.CMD",
            "C:\\s\\run.cmd.",
            "tool",
            "C:\\x y\\run.bat ",
        ] {
            for arg in [
                r#"x" & calc & ""#,
                "a&b",
                "a|b",
                "a<b",
                "a>b",
                "a^b",
                "%PATH%",
                "q\"r",
            ] {
                assert_eq!(
                    check(cmd(command, &[arg])),
                    Err(InvalidReason::Args),
                    "{command} {arg}"
                );
                assert!(batch_unsafe_args(command, &[arg.to_string()]));
            }
            assert_eq!(
                check(cmd(command, &["plain", "two words"])),
                Ok(()),
                "{command}"
            );
        }
        // Real executables may carry these characters: they are plain data for CreateProcess.
        for command in [r"C:\t\a.exe", r"C:\t\a.com"] {
            assert_eq!(check(cmd(command, &["a&b", "%x%"])), Ok(()), "{command}");
        }
        // Everything else may end up in cmd.exe (scripts, shortcuts, odd names): strict check.
        for command in [r"C:\a\go.ps1", r"C:\a\link.lnk", r"C:\a\tool.v2"] {
            assert_eq!(
                check(cmd(command, &["a&b"])),
                Err(InvalidReason::Args),
                "{command}"
            );
        }
        assert!(needs_cmd_safe_args(r"C:\dir.with.dots\tool"));
        assert!(!needs_cmd_safe_args(r"C:\dir.bat\tool.exe"));
    }

    fn shared_cases() -> serde_json::Value {
        serde_json::from_str(include_str!(
            "../../../contract-fixtures/hotlist/command-cases.json"
        ))
        .unwrap()
    }

    /// Same vectors as the frontend (`src/lib/hotlist/commandSafety.test.ts`).
    #[test]
    fn commands_must_be_local_programs_not_uris_or_network_paths() {
        let cases = shared_cases();
        let rejected = cases["rejectCommands"].as_array().unwrap();
        assert!(rejected.len() >= 20);
        for bad in rejected {
            let bad = bad.as_str().unwrap();
            assert_eq!(check(cmd(bad, &[])), Err(InvalidReason::Command), "{bad}");
        }
        for ok in cases["acceptCommands"].as_array().unwrap() {
            let ok = ok.as_str().unwrap();
            assert_eq!(check(cmd(ok, &[])), Ok(()), "{ok}");
        }
        // Folders may still live on the network (the user's own navigation choice).
        let unc = format!("{0}{0}nas{0}media", '\\');
        assert_eq!(check(dir("n", &unc)), Ok(()));
    }

    #[test]
    fn argument_metacharacters_follow_the_shared_vectors() {
        for case in shared_cases()["batchArgs"].as_array().unwrap() {
            let command = case["command"].as_str().unwrap();
            let arg = case["arg"].as_str().unwrap().to_string();
            assert_eq!(
                batch_unsafe_args(command, std::slice::from_ref(&arg)),
                case["unsafe"].as_bool().unwrap(),
                "{command} {arg}"
            );
        }
    }

    #[test]
    fn enforces_depth_count_and_field_limits() {
        let ok = HotlistDoc {
            items: vec![nest(MAX_DEPTH)],
            ..Default::default()
        };
        assert_eq!(ok.validate(), Ok(()));
        let deep = HotlistDoc {
            items: vec![nest(MAX_DEPTH + 1)],
            ..Default::default()
        };
        assert_eq!(deep.validate(), Err(InvalidReason::TooDeep));
        let many = HotlistDoc {
            items: vec![Item::Separator; MAX_ITEMS + 1],
            ..Default::default()
        };
        assert_eq!(many.validate(), Err(InvalidReason::TooMany));
        for (item, reason) in [
            (dir("", "C:\\"), InvalidReason::Title),
            (dir("a\nb", "C:\\"), InvalidReason::Title),
            (dir("a", ""), InvalidReason::Path),
            (dir("a", "C:\\a\0b"), InvalidReason::Path),
            (
                Item::Command {
                    title: "c".into(),
                    command: " ".into(),
                    args: vec![],
                },
                InvalidReason::Command,
            ),
            (
                Item::Command {
                    title: "c".into(),
                    command: "a.exe".into(),
                    args: vec!["x\0".into()],
                },
                InvalidReason::Args,
            ),
            (
                Item::Command {
                    title: "c".into(),
                    command: "a.exe".into(),
                    args: vec!["x".into(); MAX_ARGS + 1],
                },
                InvalidReason::Args,
            ),
        ] {
            let doc = HotlistDoc {
                items: vec![item],
                ..Default::default()
            };
            assert_eq!(doc.validate(), Err(reason));
        }
    }

    #[test]
    fn finds_commands_anywhere_in_the_tree_by_exact_program_and_args() {
        let doc = HotlistDoc {
            items: vec![Item::Submenu {
                title: "s".into(),
                items: vec![Item::Command {
                    title: "k".into(),
                    command: "C:\\k.exe".into(),
                    args: vec!["-a".into()],
                }],
            }],
            ..Default::default()
        };
        assert!(doc.contains_command("C:\\k.exe", &["-a".into()]));
        assert!(!doc.contains_command("C:\\k.exe", &[]));
        assert!(!doc.contains_command("C:\\other.exe", &["-a".into()]));
    }

    /// FER-467 H1: a folder item can never be a device / NT namespace path, in any spelling.
    #[test]
    fn dir_items_refuse_device_paths_in_every_spelling() {
        for bad in [
            r"\\.\UNC\evil\share",
            r"\\?\UNC\evil\share",
            r"\\?\GLOBALROOT\Device\Mup\evil\share",
            r"\\.\GLOBALROOT\Device\Mup\evil\share",
            r"//?/globalroot/device/mup/evil/share",
            r"\??\UNC\evil\share",
            r"/??/C:\x",
            r"\Device\Mup\evil\share",
            r"C:\x\..\Device\Mup\evil",
            r"\\.\pipe\x",
            r"\\.\C:\x",
            r"\\.",
            r"\\?",
            r"\\?\C:\x:stream",
            r"\\?\Volume{00000000-0000-0000-0000-000000000000}\x",
        ] {
            assert!(is_device_path(bad), "{bad}");
            for doc in [
                HotlistDoc {
                    items: vec![dir("d", bad)],
                    ..Default::default()
                },
                HotlistDoc {
                    items: vec![Item::Dir {
                        title: "d".into(),
                        path: "C:\\ok".into(),
                        target_path: Some(bad.into()),
                    }],
                    ..Default::default()
                },
            ] {
                assert_eq!(doc.validate(), Err(InvalidReason::Path), "{bad}");
            }
        }
        for good in [
            r"C:\Users\me",
            r"\\server\share\x",
            r"//server/share",
            r"\\host@SSL\DavWWWRoot\x",
            r"\\?\C:\a very\long\path",
            r"D:\devices\x",
            r"C:\Device\x",
            r"C:\globalroot\x",
        ] {
            assert!(!is_device_path(good), "{good}");
            let doc = HotlistDoc {
                items: vec![dir("d", good)],
                ..Default::default()
            };
            assert!(doc.validate().is_ok(), "{good}");
        }
    }
}
