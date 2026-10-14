//! Directory Hotlist backend (FER-458): schema v2 model, storage at a movable location, Total
//! Commander `[DirMenu]` import and `command` item launching. Tauri commands are thin wrappers
//! over Tauri-free functions so everything is unit tested.
//!
//! Errors reaching the frontend are safe kinds only, never paths.

pub mod model;
pub mod store;
pub mod tc_import;

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{AppHandle, State};

use model::HotlistDoc;
use store::{Mode, Store, StoreError, View};
use tc_import::{ImportError, ImportResult, MAX_INI_BYTES};

#[derive(Debug, Serialize, PartialEq, Eq)]
pub struct CommandError {
    pub kind: &'static str,
}

impl From<StoreError> for CommandError {
    fn from(e: StoreError) -> Self {
        Self { kind: e.kind() }
    }
}

fn err(kind: &'static str) -> CommandError {
    CommandError { kind }
}

fn config_dir(app: &AppHandle) -> PathBuf {
    crate::profile::config_dir(app).unwrap_or_else(std::env::temp_dir)
}

/// Default wincmd.ini: `%APPDATA%\GHISLER\wincmd.ini`.
pub fn default_wincmd_path() -> Option<PathBuf> {
    std::env::var_os("APPDATA")
        .filter(|v| !v.is_empty())
        .map(|a| PathBuf::from(a).join("GHISLER").join("wincmd.ini"))
}

/// Read-only import preview. `path` = the ini the user chose; `None` = the default location.
pub fn import_preview_from(path: &Path) -> Result<ImportResult, CommandError> {
    let text = path.to_str().ok_or_else(|| err("invalid_path"))?;
    // Device paths (`\\.\COM1`, `\\?\GLOBALROOT\...`) are never an ini the user means.
    if !crate::is_plausible_path(text) || text.starts_with(r"\\.\") || text.starts_with(r"\\?\") {
        return Err(err("invalid_path"));
    }
    let meta = fs::metadata(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => err("not_found"),
        _ => err("unreadable"),
    })?;
    if !meta.is_file() || meta.len() > MAX_INI_BYTES {
        return Err(err("unreadable"));
    }
    let bytes = fs::read(path).map_err(|_| err("unreadable"))?;
    let result = tc_import::parse_dirmenu(&tc_import::decode_ini(&bytes)).map_err(|e| match e {
        ImportError::NoDirMenu => err("no_dirmenu"),
    })?;
    // Whatever the preview shows must also be saveable: never hand out a document the store would
    // refuse.
    result.doc.validate().map_err(|_| err("invalid_doc"))?;
    Ok(result)
}

#[derive(Debug, PartialEq, Eq)]
pub enum RunOutcome {
    Launched,
    /// The user said no in the native confirmation: nothing ran, nothing was approved.
    Canceled,
}

/// Which item the UI wants to run: its index path in the tree plus what the UI showed for it.
/// The backend runs only if the stored hotlist has exactly that item at exactly that place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunRequest {
    pub position: Vec<usize>,
    pub title: String,
    pub command: String,
    pub args: Vec<String>,
}

/// A first run (or a changed item) needs the user's OK in a native dialog; this is what it says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// Approval key of the item as shown (FER-467 R4).
    pub key: String,
    /// Dialog text, built here from the STORED item, never from webview-supplied text.
    pub text: String,
}

/// Everything that must hold before a launch; nothing is started here.
#[derive(Debug, PartialEq, Eq)]
pub enum RunPrep {
    Ready,
    NeedsConfirm(Prompt),
}

pub const CONFIRM_CAPTION: &str = "Ferrum - run a hotlist command?";

/// How the program and its arguments reach ShellExecute (mirrors `commandLine` in
/// `src/lib/hotlist/model.ts`).
pub fn command_line(command: &str, args: &[String]) -> String {
    let program = if command.chars().any(|c| c.is_whitespace() || c == '"') {
        format!("\"{command}\"")
    } else {
        command.to_string()
    };
    std::iter::once(program)
        .chain(args.iter().map(|a| crate::shell_open::quote_arg(a)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Longest shown form of one argument (or the program / menu / file line) in the dialog.
pub const SHOWN_PART_CHARS: usize = 200;
/// Upper bound for the whole dialog text, so a message box never pushes the important lines
/// (warning, program, question) off screen.
pub const PROMPT_MAX_CHARS: usize = 2000;

/// Display-only shortening: `max` characters, then `… (N characters)`. What runs is never
/// shortened.
fn shorten(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max).collect();
    format!("{head}… ({n} characters)")
}

/// FER-467 H2: the dialog text. Order: warning (scripts, shortcuts), program, then the arguments
/// one per line (each shortened for display to `SHOWN_PART_CHARS`), menu and file, the question.
/// The whole text stays within about `PROMPT_MAX_CHARS`: arguments that do not fit are counted,
/// not shown.
fn prompt_text(
    location: &str,
    trail: &[&str],
    title: &str,
    command: &str,
    args: &[String],
) -> String {
    let menu = trail
        .iter()
        .chain(std::iter::once(&title))
        .map(|t| model::plain_title(t))
        .collect::<Vec<_>>()
        .join(" > ");
    let warning = match model::command_risk(command) {
        Some(model::Risk::Script) => {
            "WARNING: this starts a script. Scripts can do anything your account can. Only run it if you know where it came from.\n\n"
        }
        Some(model::Risk::Shortcut) => {
            "WARNING: this is a shortcut. What it really starts is hidden inside the file. Only run it if you trust it.\n\n"
        }
        None => "",
    };
    let head = format!(
        "{warning}The hotlist item \"{}\" starts this program:\n{}\n",
        shorten(&model::plain_title(title), SHOWN_PART_CHARS),
        shorten(command, SHOWN_PART_CHARS),
    );
    let tail = format!(
        "\nMenu: {}\nHotlist file: {}\n\nRun it?\n\nFerrum asks the first time, and again whenever the command, its arguments, its title, its place in the menu or the hotlist file changes.",
        shorten(&menu, SHOWN_PART_CHARS),
        shorten(location, SHOWN_PART_CHARS),
    );
    let mut body = String::new();
    if args.is_empty() {
        body.push_str("No arguments.\n");
    } else {
        body.push_str(&format!(
            "Arguments ({}), each as the program receives it:\n",
            args.len()
        ));
        let budget = PROMPT_MAX_CHARS.saturating_sub(head.chars().count() + tail.chars().count());
        let mut used = body.chars().count();
        for (i, a) in args.iter().enumerate() {
            let line = format!(
                "  {}. {}\n",
                i + 1,
                shorten(&crate::shell_open::quote_arg(a), SHOWN_PART_CHARS)
            );
            let len = line.chars().count();
            // Keep room for the "not shown" line.
            if used + len + 48 > budget {
                body.push_str(&format!(
                    "  … and {} more argument(s) not shown\n",
                    args.len() - i
                ));
                break;
            }
            used += len;
            body.push_str(&line);
        }
    }
    format!("{head}{body}{tail}")
}

/// Gate for a command item (FER-458, FER-467 R4): the stored hotlist must have exactly the
/// requested item at exactly the requested place, batch files get no cmd metacharacters, and an
/// item whose full identity (file, position, titles on the way, program, arguments) was never
/// approved needs the native confirmation first. Approval never skips the argument check.
pub fn prepare_run(store: &Store, dir: &Path, req: &RunRequest) -> Result<RunPrep, CommandError> {
    let doc = store.current_doc(dir);
    let Some((
        model::Item::Command {
            title,
            command,
            args,
        },
        trail,
    )) = doc.item_at(&req.position)
    else {
        return Err(err("stale"));
    };
    if *title != req.title || *command != req.command || *args != req.args {
        return Err(err("stale"));
    }
    if model::batch_unsafe_args(command, args) {
        return Err(err("unsafe_args"));
    }
    let location = store::location_file(dir).ok_or_else(|| err("location_unsafe"))?;
    let location = location.to_string_lossy();
    let key = store::command_key(&store::CommandIdentity {
        location: &location,
        position: &req.position,
        trail: &trail,
        title,
        command,
        args,
        cwd: None,
    });
    if store::is_approved(dir, &key) {
        return Ok(RunPrep::Ready);
    }
    Ok(RunPrep::NeedsConfirm(Prompt {
        text: prompt_text(&location, &trail, title, command, args),
        key,
    }))
}

/// After the user said yes to `shown`: the item must still be the one that was shown (the file can
/// be reloaded while the dialog is open), then the approval is recorded.
pub fn approve_shown(
    store: &Store,
    dir: &Path,
    req: &RunRequest,
    shown: &Prompt,
) -> Result<(), CommandError> {
    match prepare_run(store, dir, req)? {
        RunPrep::Ready => Ok(()),
        RunPrep::NeedsConfirm(now) if now.key == shown.key => {
            store::approve(dir, &shown.key).map_err(Into::into)
        }
        RunPrep::NeedsConfirm(_) => Err(err("stale")),
    }
}

fn launch_error(e: crate::domain::FsError) -> CommandError {
    match e.code {
        crate::domain::ErrorCode::NotFound => err("not_found"),
        crate::domain::ErrorCode::PermissionDenied => err("permission_denied"),
        _ => err("launch_failed"),
    }
}

/// The whole flow on one thread (tests, and the shape the Tauri command follows): gate, native
/// confirmation when needed, re-check, approve, launch. Launching is a plain ShellExecute with
/// separate arguments.
pub fn run_command_in(
    store: &Store,
    dir: &Path,
    req: &RunRequest,
    confirm: impl FnOnce(&Prompt) -> bool,
    launch: impl FnOnce(&str, &[String]) -> Result<(), crate::domain::FsError>,
) -> Result<RunOutcome, CommandError> {
    if let RunPrep::NeedsConfirm(prompt) = prepare_run(store, dir, req)? {
        if !confirm(&prompt) {
            return Ok(RunOutcome::Canceled);
        }
        approve_shown(store, dir, req, &prompt)?;
    }
    launch(&req.command, &req.args).map_err(launch_error)?;
    Ok(RunOutcome::Launched)
}

/// FER-467 R5: the confirmation is a native Windows message box owned by the Ferrum window, with
/// text built by the backend. The webview can neither draw it nor answer it. Cancel is the default
/// button, so a held Enter key cannot run anything. (The shared module also serves the automation
/// confirmations; the flags and behaviour are unchanged.)
#[cfg(windows)]
fn native_confirm(owner: isize, text: &str) -> bool {
    crate::native_dialog::confirm(owner, CONFIRM_CAPTION, text)
}

#[cfg(not(windows))]
fn native_confirm(_owner: isize, _text: &str) -> bool {
    false
}

#[cfg(windows)]
fn owner_hwnd(window: &tauri::Window) -> isize {
    window.hwnd().map(|h| h.0 as isize).unwrap_or(0)
}

#[cfg(not(windows))]
fn owner_hwnd(_window: &tauri::Window) -> isize {
    0
}

#[tauri::command]
pub fn hotlist_load(app: AppHandle, store: State<'_, Store>) -> View {
    store.load(&config_dir(&app))
}

#[tauri::command]
pub fn hotlist_save(
    app: AppHandle,
    store: State<'_, Store>,
    doc: HotlistDoc,
) -> Result<View, CommandError> {
    store.save(&config_dir(&app), &doc).map_err(Into::into)
}

/// `Some(true)` = a valid hotlist file exists at the candidate, `Some(false)` = nothing there,
/// error `target_unusable` = something is there that is not a usable hotlist (or the folder is
/// not reachable).
#[tauri::command]
pub fn hotlist_check_location(app: AppHandle, path: Option<String>) -> Result<bool, CommandError> {
    let location = path.as_deref().map(Path::new);
    Store::check_location(location, &config_dir(&app))
        .map(|t| t == store::Target::Valid)
        .map_err(Into::into)
}

#[tauri::command]
pub fn hotlist_set_location(
    app: AppHandle,
    store: State<'_, Store>,
    path: Option<String>,
    mode: Mode,
) -> Result<View, CommandError> {
    let location = path.as_deref().map(Path::new);
    store
        .set_location(&config_dir(&app), location, mode)
        .map_err(Into::into)
}

#[tauri::command]
pub async fn hotlist_import_preview(path: Option<String>) -> Result<ImportResult, CommandError> {
    let path = match path.filter(|p| !p.trim().is_empty()) {
        Some(p) => PathBuf::from(p),
        None => default_wincmd_path().ok_or_else(|| err("not_found"))?,
    };
    tauri::async_runtime::spawn_blocking(move || import_preview_from(&path))
        .await
        .map_err(|_| err("unreadable"))?
}

/// Result for the UI: `launched: false` = the user canceled the native confirmation.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunResult {
    pub launched: bool,
}

/// Run the command item at `position`. The UI also sends what it showed (title, command, args);
/// the backend runs only when the stored hotlist matches exactly, asks in a native dialog when the
/// item's identity is not approved yet, and never takes a "confirmed" flag from the webview.
#[tauri::command]
pub async fn hotlist_run_command(
    app: AppHandle,
    window: tauri::Window,
    store: State<'_, Store>,
    position: Vec<usize>,
    title: String,
    command: String,
    args: Vec<String>,
) -> Result<RunResult, CommandError> {
    let dir = config_dir(&app);
    let req = RunRequest {
        position,
        title,
        command,
        args,
    };
    if let RunPrep::NeedsConfirm(prompt) = prepare_run(&store, &dir, &req)? {
        let owner = owner_hwnd(&window);
        let text = prompt.text.clone();
        let yes = tauri::async_runtime::spawn_blocking(move || native_confirm(owner, &text))
            .await
            .unwrap_or(false);
        if !yes {
            return Ok(RunResult { launched: false });
        }
        approve_shown(&store, &dir, &req, &prompt)?;
    }
    // ShellExecute can block on a slow UNC/association lookup: keep it off the async workers.
    let RunRequest { command, args, .. } = req;
    tauri::async_runtime::spawn_blocking(move || {
        crate::shell_open::launch_program(&command, &args)
    })
    .await
    .map_err(|_| err("launch_failed"))?
    .map_err(launch_error)?;
    Ok(RunResult { launched: true })
}

#[cfg(test)]
mod tests {
    use super::*;
    use model::Item;
    use std::cell::Cell;

    fn kando() -> Item {
        Item::Command {
            title: "&k".into(),
            command: "C:\\k.exe".into(),
            args: vec!["-a".into()],
        }
    }

    fn doc_with_command() -> HotlistDoc {
        HotlistDoc {
            items: vec![
                Item::Dir {
                    title: "&1 Home".into(),
                    path: "C:\\".into(),
                    target_path: None,
                },
                Item::Submenu {
                    title: "&Tools".into(),
                    items: vec![kando()],
                },
            ],
            ..Default::default()
        }
    }

    fn req(position: &[usize]) -> RunRequest {
        RunRequest {
            position: position.to_vec(),
            title: "&k".into(),
            command: "C:\\k.exe".into(),
            args: vec!["-a".into()],
        }
    }

    fn ready(dir: &Path, doc: &HotlistDoc) -> Store {
        let store = Store::new();
        store.load(dir);
        store.save(dir, doc).unwrap();
        store
    }

    /// Runs `r` with a confirm that answers `answer`; returns (outcome, asked, launched).
    fn run(store: &Store, dir: &Path, r: &RunRequest, answer: bool) -> (RunOutcome, bool, bool) {
        let asked = Cell::new(false);
        let launched = Cell::new(false);
        let out = run_command_in(
            store,
            dir,
            r,
            |_| {
                asked.set(true);
                answer
            },
            |_, _| {
                launched.set(true);
                Ok(())
            },
        )
        .unwrap();
        (out, asked.get(), launched.get())
    }

    #[test]
    fn commands_need_the_native_confirmation_once_and_only_then_launch() {
        let cfg = tempfile::tempdir().unwrap();
        let store = ready(cfg.path(), &doc_with_command());
        // No: nothing runs, nothing is remembered.
        assert_eq!(
            run(&store, cfg.path(), &req(&[1, 0]), false),
            (RunOutcome::Canceled, true, false)
        );
        assert!(!cfg.path().join(store::APPROVALS_NAME).exists());
        // Yes: runs, and is remembered.
        assert_eq!(
            run(&store, cfg.path(), &req(&[1, 0]), true),
            (RunOutcome::Launched, true, true)
        );
        assert_eq!(
            run(&store, cfg.path(), &req(&[1, 0]), false),
            (RunOutcome::Launched, false, true)
        );
    }

    #[test]
    fn the_dialog_text_comes_from_the_stored_item() {
        let cfg = tempfile::tempdir().unwrap();
        let store = ready(cfg.path(), &doc_with_command());
        let RunPrep::NeedsConfirm(p) = prepare_run(&store, cfg.path(), &req(&[1, 0])).unwrap()
        else {
            panic!("needs confirm");
        };
        assert!(p.text.contains("Menu: Tools > k"), "{}", p.text);
        assert!(p.text.contains("program:\nC:\\k.exe\n"), "{}", p.text);
        assert!(p.text.contains("  1. -a\n"), "{}", p.text);
        assert!(p.text.contains(store::FILE_NAME), "{}", p.text);
        assert!(!p.text.contains("WARNING"));
        let script = prompt_text("C:\\h.json", &[], "s", "C:\\x.bat", &[]);
        assert!(script.starts_with("WARNING: this starts a script"));
        let lnk = prompt_text("C:\\h.json", &[], "s", "C:\\x.LNK", &[]);
        assert!(lnk.starts_with("WARNING: this is a shortcut"));
        assert_eq!(
            command_line("C:\\a b\\x.exe", &["p q".into(), "r".into()]),
            r#""C:\a b\x.exe" "p q" r"#
        );
    }

    /// FER-467 H2: huge arguments cannot push the warning, the program or the question off the
    /// dialog; display is shortened, what runs is not.
    #[test]
    fn the_dialog_text_puts_the_warning_and_program_first_and_stays_bounded() {
        let long = "x".repeat(4096);
        let args: Vec<String> = (0..32).map(|_| long.clone()).collect();
        let text = prompt_text("C:\\h.json", &["&Tools"], "&run", "C:\\s\\go.cmd", &args);
        assert!(text.starts_with("WARNING: this starts a script"), "{text}");
        let program_at = text.find("C:\\s\\go.cmd").unwrap();
        let args_at = text.find("Arguments (32)").unwrap();
        assert!(program_at < args_at);
        assert!(text.contains(&format!("  1. {}… (4096 characters)\n", "x".repeat(200))));
        assert!(text.contains("more argument(s) not shown"), "{text}");
        assert!(text.contains("Run it?"));
        assert!(
            text.chars().count() <= PROMPT_MAX_CHARS + 50,
            "{}",
            text.chars().count()
        );
        // A short argument list is shown whole, in order, quoted like the backend sends it.
        let small = prompt_text(
            "C:\\h.json",
            &[],
            "t",
            "C:\\t.exe",
            &["a b".into(), "c".into()],
        );
        assert!(small.contains("  1. \"a b\"\n  2. c\n"), "{small}");
        assert!(!small.contains("not shown"));
        let none = prompt_text("C:\\h.json", &[], "t", "C:\\t.exe", &[]);
        assert!(none.contains("No arguments."));
        assert_eq!(shorten("abc", 3), "abc");
        assert_eq!(shorten("abcd", 3), "abc… (4 characters)");
    }

    #[test]
    fn a_long_command_line_still_launches_whole() {
        let cfg = tempfile::tempdir().unwrap();
        let long = "y".repeat(4000);
        let doc = HotlistDoc {
            items: vec![Item::Command {
                title: "l".into(),
                command: "C:\\l.exe".into(),
                args: vec![long.clone()],
            }],
            ..Default::default()
        };
        let store = ready(cfg.path(), &doc);
        let r = RunRequest {
            position: vec![0],
            title: "l".into(),
            command: "C:\\l.exe".into(),
            args: vec![long.clone()],
        };
        let got = Cell::new(0usize);
        run_command_in(
            &store,
            cfg.path(),
            &r,
            |p| {
                assert!(p.text.contains("(4000 characters)"));
                true
            },
            |_, a| {
                got.set(a[0].len());
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(got.get(), 4000);
    }

    #[test]
    fn only_the_item_at_the_requested_place_can_run() {
        let cfg = tempfile::tempdir().unwrap();
        let store = ready(cfg.path(), &doc_with_command());
        let launched = Cell::new(false);
        let mut bad = vec![req(&[0]), req(&[1]), req(&[1, 1]), req(&[2, 0]), req(&[])];
        bad.push(RunRequest {
            command: "C:\\evil.exe".into(),
            ..req(&[1, 0])
        });
        bad.push(RunRequest {
            args: vec![],
            ..req(&[1, 0])
        });
        bad.push(RunRequest {
            args: vec!["-a".into(), "extra".into()],
            ..req(&[1, 0])
        });
        bad.push(RunRequest {
            title: "&1 k".into(),
            ..req(&[1, 0])
        });
        for r in bad {
            let e = run_command_in(
                &store,
                cfg.path(),
                &r,
                |_| true,
                |_, _| {
                    launched.set(true);
                    Ok(())
                },
            )
            .unwrap_err();
            assert_eq!(e.kind, "stale", "{r:?}");
        }
        assert!(!launched.get());
    }

    /// FER-467 R4: someone who edits the synced hotlist cannot move an approved command to another
    /// accelerator / place, or change its arguments, and keep the approval.
    #[test]
    fn approval_is_lost_when_args_position_or_titles_change() {
        let cfg = tempfile::tempdir().unwrap();
        let store = ready(cfg.path(), &doc_with_command());
        run(&store, cfg.path(), &req(&[1, 0]), true);

        // 1. Arguments changed in place.
        let mut changed = doc_with_command();
        if let Item::Submenu { items, .. } = &mut changed.items[1]
            && let Item::Command { args, .. } = &mut items[0]
        {
            args.push("--new".into());
        }
        store.load(cfg.path());
        store.save(cfg.path(), &changed).unwrap();
        let new_args = RunRequest {
            args: vec!["-a".into(), "--new".into()],
            ..req(&[1, 0])
        };
        assert_eq!(
            run(&store, cfg.path(), &new_args, false),
            (RunOutcome::Canceled, true, false)
        );

        // 2. Same program + args moved to the top level, first place.
        let mut moved = doc_with_command();
        moved.items.insert(0, kando());
        store.save(cfg.path(), &moved).unwrap();
        assert_eq!(
            run(&store, cfg.path(), &req(&[0]), false),
            (RunOutcome::Canceled, true, false)
        );

        // 3. Same place, but the item now answers to accelerator 1 (title changed).
        let mut retitled = doc_with_command();
        if let Item::Submenu { items, .. } = &mut retitled.items[1]
            && let Item::Command { title, .. } = &mut items[0]
        {
            *title = "&1 k".into();
        }
        store.save(cfg.path(), &retitled).unwrap();
        let r = RunRequest {
            title: "&1 k".into(),
            ..req(&[1, 0])
        };
        assert_eq!(
            run(&store, cfg.path(), &r, false),
            (RunOutcome::Canceled, true, false)
        );

        // 4. Same item, the submenu above it renamed (its accelerator changes).
        let mut parent = doc_with_command();
        if let Item::Submenu { title, .. } = &mut parent.items[1] {
            *title = "&1 Tools".into();
        }
        store.save(cfg.path(), &parent).unwrap();
        assert_eq!(
            run(&store, cfg.path(), &req(&[1, 0]), false),
            (RunOutcome::Canceled, true, false)
        );

        // Back to the approved original: no question.
        store.save(cfg.path(), &doc_with_command()).unwrap();
        assert_eq!(
            run(&store, cfg.path(), &req(&[1, 0]), false),
            (RunOutcome::Launched, false, true)
        );
    }

    #[test]
    fn a_hotlist_changed_while_the_dialog_was_open_is_not_approved() {
        let cfg = tempfile::tempdir().unwrap();
        let store = ready(cfg.path(), &doc_with_command());
        let mut swapped = doc_with_command();
        if let Item::Submenu { title, .. } = &mut swapped.items[1] {
            *title = "&Other".into();
        }
        let launched = Cell::new(false);
        let e = run_command_in(
            &store,
            cfg.path(),
            &req(&[1, 0]),
            |_| {
                // Another PC's change is picked up while the user reads the dialog.
                store.save(cfg.path(), &swapped).unwrap();
                true
            },
            |_, _| {
                launched.set(true);
                Ok(())
            },
        )
        .unwrap_err();
        assert_eq!(e.kind, "stale");
        assert!(!launched.get());
        assert!(!cfg.path().join(store::APPROVALS_NAME).exists());
    }

    #[cfg(windows)]
    #[test]
    fn an_approval_does_not_follow_the_hotlist_to_another_file() {
        let cfg = tempfile::tempdir().unwrap();
        let other = tempfile::tempdir().unwrap();
        let store = ready(cfg.path(), &doc_with_command());
        run(&store, cfg.path(), &req(&[1, 0]), true);
        store
            .set_location(
                cfg.path(),
                Some(&other.path().join("hotlist.json")),
                store::Mode::Move,
            )
            .unwrap();
        assert_eq!(
            run(&store, cfg.path(), &req(&[1, 0]), false),
            (RunOutcome::Canceled, true, false)
        );
    }

    #[test]
    fn import_preview_reads_the_fixture_without_modifying_it() {
        let dir = tempfile::tempdir().unwrap();
        let ini = dir.path().join("wincmd.ini");
        let text = include_str!("../../../contract-fixtures/hotlist/tc-dirmenu.ini");
        fs::write(&ini, text).unwrap();
        let before = fs::read(&ini).unwrap();
        let r = import_preview_from(&ini).unwrap();
        assert!(r.warnings.is_empty());
        assert_eq!(r.doc.items.len(), 6);
        assert_eq!(fs::read(&ini).unwrap(), before);
        // Import never produces approvals.
        assert!(!dir.path().join(store::APPROVALS_NAME).exists());
    }

    #[test]
    fn import_preview_reports_safe_error_kinds() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            import_preview_from(&dir.path().join("none.ini"))
                .unwrap_err()
                .kind,
            "not_found"
        );
        let ini = dir.path().join("w.ini");
        fs::write(&ini, "[Configuration]\nx=1\n").unwrap();
        assert_eq!(import_preview_from(&ini).unwrap_err().kind, "no_dirmenu");
        assert_eq!(
            import_preview_from(dir.path()).unwrap_err().kind,
            "unreadable"
        );
        assert_eq!(
            import_preview_from(Path::new(" ")).unwrap_err().kind,
            "invalid_path"
        );
        for device in [r"\\.\COM1", r"\\?\GLOBALROOT\Device\x"] {
            assert_eq!(
                import_preview_from(Path::new(device)).unwrap_err().kind,
                "invalid_path"
            );
        }
    }
}
