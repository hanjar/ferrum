//! Persisted preferences (backlog polish): JSON at the app config dir,
//! atomic write, validated on load. Tauri-free inner functions.
//!
//! FER-370 APP-03: Appearance fields are additive with `#[serde(default)]`
//! on the container — old configs without the new fields load with the
//! struct defaults (which mirror the pre-Appearance behavior), and the
//! three legacy palette IDs stay valid alongside the six new-schema IDs.

use crate::domain::FsError;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

pub const FILE_NAME: &str = "config.json";

/// FER-406 pane split bounds; anything outside (or non-finite) resets to 0.5.
pub const PANE_SPLIT_MIN: f64 = 0.2;
pub const PANE_SPLIT_MAX: f64 = 0.8;
const PANE_SPLIT_DEFAULT: f64 = 0.5;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Prefs {
    /// dark | light | system
    pub theme: String,
    /// violet | electric | aqua | green | yellow | orange
    pub palette: String,
    pub show_hidden: bool,
    /// F2-25 terminal preference: auto | wt | cmd | powershell
    pub terminal: String,
    /// Left pane share of the dual-pane grid (FER-406), kept in [0.2, 0.8].
    pub pane_split: f64,
    /// FER-395 tab behavior (Settings > Tabs); absent fields take defaults.
    pub tabs: TabsPrefs,
    /// FER-395 last launch session; None while restore is off.
    pub session: Option<SessionSnapshot>,
    /// FER-396 preview panel behavior (Settings > Preview); absent fields take defaults.
    pub preview: PreviewPrefs,
    /// FER-370: compact | comfortable (row 26/30 px, token --row)
    pub density: String,
    /// FER-370: colored | monochrome (file glyph color only)
    pub icon_style: String,
    /// FER-478: file icon shape: Line | Duotone | Label | Tile
    pub icon_theme: String,
    /// FER-370/D50: named text scale level (s | m | l | xl)
    pub ui_font_level: String,
    /// FER-370: display-only extension hiding
    pub show_extensions: bool,
    /// FER-370: folders before files regardless of sort direction
    pub folders_first: bool,
    /// FER-460: offer the elevated fast NTFS scan before a Disk usage scan.
    /// Only the OFFER is remembered — elevation itself is never automatic.
    pub disk_usage_fast_offer: bool,
    /// FER-464: a cached Disk usage scan older than this many hours carries a
    /// warning (1..=8760; invalid -> 24).
    pub disk_usage_cache_warn_hours: u32,
    /// FER-491: address bar / breadcrumb on its own full-width row above the
    /// pane toolbar. Off by default (single compact row).
    pub address_own_row: bool,
    /// FER-477a: show folder sizes from the FerrumIndexer in the Size column
    /// (only answered in its High performance mode). Off by default.
    pub indexer_folder_sizes: bool,
    /// FER-477b: two-way sync by content may skip reading files that the
    /// FerrumIndexer journal proves unchanged. Opt-in (security review):
    /// off by default.
    pub sync_journal_skip: bool,
    pub confirm_before_deleting: bool,
    pub verify_after_copy: bool,
    /// JSON key is `copyBufferKiB` (the frontend contract); camelCase alone
    /// would emit `copyBufferKib`, which the UI never reads or writes.
    #[serde(rename = "copyBufferKiB", alias = "copyBufferKib")]
    pub copy_buffer_kib: u32,
    /// FER-471: default copy worker count. 0 = automatic (adaptive), 1..=16 fixed.
    pub copy_threads: u32,
    /// FER-471: flush every copied file to disk before it is published ("Tulis aman"). Off by
    /// default; Move always does it regardless.
    pub copy_safe_write: bool,
    /// FER-471: default copy priority: `normal` | `background`.
    pub copy_priority: String,
    pub conflict_policy: String,
    pub preserve_timestamps: bool,
    pub preserve_permissions: bool,
    /// FER-505: optional path to user's Rar.exe for creating RAR archives (default None).
    #[serde(default)]
    pub rar_exe_path: Option<String>,
}

/// FER-395 tab behavior: how tabs open, close, size and persist.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TabsPrefs {
    /// New tabs open right after the ACTIVE tab instead of at the end.
    pub open_next_to_current: bool,
    /// Reopen the last session's tabs per pane at launch.
    pub restore_tabs: bool,
    /// fit | fixed
    pub tab_width: String,
    /// Middle-click (auxclick button 1) closes the clicked tab.
    pub close_on_middle_click: bool,
}

impl Default for TabsPrefs {
    fn default() -> Self {
        Self {
            open_next_to_current: true,
            restore_tabs: true,
            tab_width: "fit".into(),
            close_on_middle_click: true,
        }
    }
}

/// Allowed text-preview byte limits (decimal 100 KB .. 4 MB).
pub const PREVIEW_TEXT_LIMITS: [u64; 5] = [100_000, 500_000, 1_000_000, 2_000_000, 4_000_000];
const PREVIEW_TEXT_DEFAULT: u64 = 2_000_000;

/// FER-396 preview behavior: what the Quick View panel renders.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct PreviewPrefs {
    /// Show the Quick View panel when the app starts.
    pub open_on_launch: bool,
    /// Larger text files show metadata only; one of PREVIEW_TEXT_LIMITS.
    pub max_text_bytes: u64,
    pub syntax_highlight: bool,
    pub autoplay_video: bool,
    pub read_metadata: bool,
}

impl Default for PreviewPrefs {
    fn default() -> Self {
        Self {
            open_on_launch: false,
            max_text_bytes: PREVIEW_TEXT_DEFAULT,
            syntax_highlight: true,
            autoplay_video: false,
            read_metadata: true,
        }
    }
}

/// One pane's restored session: tab paths in order + active tab index.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct PaneSession {
    pub tabs: Vec<String>,
    pub current: usize,
}

/// Both panes' restored sessions (FER-395).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct SessionSnapshot {
    pub panes: Vec<PaneSession>,
}

pub const MAX_SESSION_PANES: usize = 2;
pub const MAX_SESSION_TABS: usize = 12;

impl Default for Prefs {
    fn default() -> Self {
        Self {
            theme: "dark".into(),
            palette: "violet".into(),
            show_hidden: false,
            terminal: "auto".into(),
            pane_split: PANE_SPLIT_DEFAULT,
            tabs: TabsPrefs::default(),
            session: None,
            preview: PreviewPrefs::default(),
            // Appearance defaults mirror the pre-Appearance behavior exactly,
            // so old users see no change after the migration.
            density: "compact".into(),
            icon_style: "colored".into(),
            icon_theme: "Tile".into(),
            ui_font_level: "m".into(),
            show_extensions: true,
            folders_first: true,
            disk_usage_fast_offer: true,
            disk_usage_cache_warn_hours: 24,
            address_own_row: false,
            indexer_folder_sizes: false,
            sync_journal_skip: false,
            confirm_before_deleting: true,
            verify_after_copy: false,
            copy_buffer_kib: 1024,
            copy_threads: 0,
            copy_safe_write: false,
            copy_priority: "normal".into(),
            conflict_policy: "ask".into(),
            preserve_timestamps: true,
            preserve_permissions: false,
            rar_exe_path: None,
        }
    }
}

fn valid_theme(s: &str) -> bool {
    matches!(s, "dark" | "light" | "system")
}

fn valid_palette(s: &str) -> bool {
    matches!(
        s,
        "violet" | "electric" | "aqua" | "green" | "yellow" | "orange"
    )
}

fn valid_terminal(s: &str) -> bool {
    matches!(s, "auto" | "wt" | "cmd" | "powershell")
}

fn valid_density(s: &str) -> bool {
    matches!(s, "compact" | "comfortable")
}

fn valid_icon_style(s: &str) -> bool {
    matches!(s, "colored" | "monochrome")
}

fn valid_icon_theme(s: &str) -> bool {
    matches!(s, "Line" | "Duotone" | "Label" | "Tile")
}

fn valid_font_level(s: &str) -> bool {
    matches!(s, "s" | "m" | "l" | "xl")
}

fn valid_pane_split(v: f64) -> bool {
    v.is_finite() && (PANE_SPLIT_MIN..=PANE_SPLIT_MAX).contains(&v)
}

fn normalize_pane_split(v: f64) -> f64 {
    if valid_pane_split(v) {
        v
    } else {
        PANE_SPLIT_DEFAULT
    }
}

fn valid_tab_width(s: &str) -> bool {
    matches!(s, "fit" | "fixed")
}

/// FER-395: session snapshots validate whole — too many panes/tabs are
/// capped, empty paths drop out, out-of-range currents clamp to the first
/// tab, and a snapshot with nothing restorable becomes None. Restore then
/// falls back to the default boot, never a partial or broken tab set.
fn sanitize_session(s: Option<SessionSnapshot>) -> Option<SessionSnapshot> {
    let snap = s?;
    let mut panes = Vec::new();
    for pane in snap.panes.into_iter().take(MAX_SESSION_PANES) {
        let tabs: Vec<String> = pane
            .tabs
            .into_iter()
            .filter(|p| !p.trim().is_empty())
            .take(MAX_SESSION_TABS)
            .collect();
        if tabs.is_empty() {
            continue;
        }
        let current = if pane.current < tabs.len() {
            pane.current
        } else {
            0
        };
        panes.push(PaneSession { tabs, current });
    }
    if panes.is_empty() {
        None
    } else {
        Some(SessionSnapshot { panes })
    }
}

fn valid_conflict_policy(s: &str) -> bool {
    matches!(s, "ask" | "overwrite" | "skip" | "keepBoth")
}

/// Mirrors `copy_pool::MAX_THREADS`.
const COPY_THREADS_MAX: u32 = 16;
const COPY_BUFFER_MIN_KIB: u32 = 64;
const COPY_BUFFER_MAX_KIB: u32 = 8192;

impl Prefs {
    /// Field-wise normalization (FER-395 + FER-370): any invalid value falls
    /// back to that field's default; untouched fields always survive.
    fn normalize(&mut self) {
        if !valid_theme(&self.theme) {
            self.theme = "dark".into();
        }
        if !valid_palette(&self.palette) {
            self.palette = "violet".into();
        }
        if !valid_terminal(&self.terminal) {
            self.terminal = "auto".into();
        }
        self.pane_split = normalize_pane_split(self.pane_split);
        if !valid_tab_width(&self.tabs.tab_width) {
            self.tabs.tab_width = "fit".into();
        }
        if !PREVIEW_TEXT_LIMITS.contains(&self.preview.max_text_bytes) {
            self.preview.max_text_bytes = PREVIEW_TEXT_DEFAULT;
        }
        self.session = sanitize_session(self.session.take());
        if !valid_density(&self.density) {
            self.density = "compact".into();
        }
        if !valid_icon_style(&self.icon_style) {
            self.icon_style = "colored".into();
        }
        if !valid_icon_theme(&self.icon_theme) {
            self.icon_theme = "Tile".into();
        }
        if !valid_font_level(&self.ui_font_level) {
            self.ui_font_level = "m".into();
        }
        if !valid_conflict_policy(&self.conflict_policy) {
            self.conflict_policy = "ask".into();
        }
        self.copy_buffer_kib = self
            .copy_buffer_kib
            .clamp(COPY_BUFFER_MIN_KIB, COPY_BUFFER_MAX_KIB);
        self.copy_threads = self.copy_threads.min(COPY_THREADS_MAX);
        if !(1..=8760).contains(&self.disk_usage_cache_warn_hours) {
            self.disk_usage_cache_warn_hours = 24;
        }
        if !matches!(self.copy_priority.as_str(), "normal" | "background") {
            self.copy_priority = "normal".into();
        }
        if let Some(path) = &self.rar_exe_path {
            let trimmed = path.trim();
            if trimmed.is_empty() {
                self.rar_exe_path = None;
            } else {
                self.rar_exe_path = Some(trimmed.to_string());
            }
        }
    }
}

/// FER-103: a wrongly TYPED value anywhere in `preview` (a string for a bool,
/// a float/negative/huge `maxTextBytes`, ...) or a non-object `preview` would
/// fail deserialization of the whole file and silently reset EVERY pref.
/// Drop only the bad field here so `serde(default)` and `normalize` restore
/// just that value. Schema-driven: every `preview` field is checked.
fn sanitize_preview_json(obj: &mut serde_json::Map<String, serde_json::Value>) {
    const BOOL_FIELDS: [&str; 4] = [
        "openOnLaunch",
        "syntaxHighlight",
        "autoplayVideo",
        "readMetadata",
    ];
    let Some(preview) = obj.get_mut("preview") else {
        return;
    };
    let Some(pv) = preview.as_object_mut() else {
        obj.remove("preview");
        return;
    };
    for key in BOOL_FIELDS {
        if pv.get(key).is_some_and(|v| !v.is_boolean()) {
            pv.remove(key);
        }
    }
    let ok = pv
        .get("maxTextBytes")
        .and_then(|v| v.as_u64())
        .is_some_and(|n| PREVIEW_TEXT_LIMITS.contains(&n));
    if !ok {
        pv.remove("maxTextBytes");
    }
}

/// Load with validation; any garbage falls back field-wise to defaults.
pub fn load_from(dir: &Path) -> Prefs {
    let raw = match fs::read_to_string(dir.join(FILE_NAME)) {
        Ok(s) => s,
        Err(_) => return Prefs::default(),
    };
    // D50 migration: configs written before the named font levels carry
    // `uiFontSize` as a NUMBER (12|13|14). Map it onto the new field before
    // deserializing; shapes we cannot understand still hit the normal path.
    let mut value: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(v) => v,
        Err(_) => return Prefs::default(),
    };
    if let Some(obj) = value.as_object_mut() {
        if !obj.contains_key("uiFontLevel") {
            let level = match obj.get("uiFontSize").and_then(|v| v.as_u64()) {
                Some(12) => Some("s"),
                Some(14) => Some("l"),
                Some(13) => Some("m"),
                _ => None,
            };
            if let Some(level) = level {
                obj.insert(
                    "uiFontLevel".into(),
                    serde_json::Value::String(level.into()),
                );
            }
        }
        obj.remove("uiFontSize");
        sanitize_preview_json(obj);
    }
    let parsed: Prefs = match serde_json::from_value(value) {
        Ok(p) => p,
        Err(_) => return Prefs::default(),
    };
    let mut p = parsed;
    p.normalize();
    p
}

/// Atomic write (temp + rename) so a crash never leaves a half file.
pub fn save_to(dir: &Path, prefs: &Prefs) -> Result<(), FsError> {
    let mut p = prefs.clone();
    p.normalize();
    fs::create_dir_all(dir).map_err(|e| FsError::from(e).with_path(dir.to_string_lossy()))?;
    let tmp = dir.join(format!("{FILE_NAME}.tmp-{}", std::process::id()));
    fs::write(
        &tmp,
        serde_json::to_string_pretty(&p)
            .map_err(|e| FsError::new(crate::domain::ErrorCode::Io, e.to_string()))?,
    )
    .map_err(|e| FsError::from(e).with_path(tmp.to_string_lossy()))?;
    fs::rename(&tmp, dir.join(FILE_NAME))
        .map_err(|e| FsError::from(e).with_path(dir.to_string_lossy()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ferrum-prefs-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = temp("missing");
        assert_eq!(load_from(&dir), Prefs::default());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn indexer_folder_sizes_defaults_off_and_old_configs_load_without_it() {
        assert!(!Prefs::default().indexer_folder_sizes);
        assert!(
            !Prefs::default().sync_journal_skip,
            "opt-in (FER-477 review)"
        );
        let dir = temp("foldersizes");
        fs::write(dir.join(FILE_NAME), r#"{"theme":"light"}"#).unwrap();
        assert!(!load_from(&dir).indexer_folder_sizes);
        let p = Prefs {
            indexer_folder_sizes: true,
            ..Prefs::default()
        };
        save_to(&dir, &p).unwrap();
        assert!(load_from(&dir).indexer_folder_sizes);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn address_own_row_defaults_off_and_old_configs_load_without_it() {
        assert!(!Prefs::default().address_own_row);
        let dir = temp("ownrow");
        fs::write(dir.join(FILE_NAME), r#"{"theme":"light"}"#).unwrap();
        let loaded = load_from(&dir);
        assert_eq!(loaded.theme, "light");
        assert!(!loaded.address_own_row);
        let p = Prefs {
            address_own_row: true,
            ..Prefs::default()
        };
        save_to(&dir, &p).unwrap();
        assert!(load_from(&dir).address_own_row);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn round_trips_and_validates_garbage() {
        let dir = temp("round");
        let p = Prefs {
            theme: "light".into(),
            palette: "aqua".into(),
            show_hidden: true,
            terminal: "wt".into(),
            pane_split: 0.65,
            tabs: TabsPrefs {
                open_next_to_current: false,
                restore_tabs: false,
                tab_width: "fixed".into(),
                close_on_middle_click: false,
            },
            session: Some(SessionSnapshot {
                panes: vec![PaneSession {
                    tabs: vec!["C:\\a".into(), "C:\\b".into()],
                    current: 1,
                }],
            }),
            preview: PreviewPrefs::default(),
            density: "comfortable".into(),
            icon_style: "monochrome".into(),
            icon_theme: "Tile".into(),
            ui_font_level: "xl".into(),
            show_extensions: false,
            folders_first: false,
            disk_usage_fast_offer: false,
            disk_usage_cache_warn_hours: 72,
            address_own_row: true,
            indexer_folder_sizes: true,
            sync_journal_skip: true,
            confirm_before_deleting: false,
            verify_after_copy: true,
            copy_buffer_kib: 2048,
            copy_threads: 8,
            copy_safe_write: true,
            copy_priority: "background".into(),
            conflict_policy: "skip".into(),
            preserve_timestamps: false,
            preserve_permissions: true,
            rar_exe_path: None,
        };
        save_to(&dir, &p).unwrap();
        assert_eq!(load_from(&dir), p);

        fs::write(
            dir.join(FILE_NAME),
            r#"{"theme":"hacker","palette":"neon","showHidden":true,"terminal":"wt","density":"cozy","iconStyle":"neon","iconTheme":"Neon","uiFontSize":99,"showExtensions":true,"foldersFirst":true}"#,
        )
        .unwrap();
        let fixed = load_from(&dir);
        assert_eq!(fixed.theme, "dark");
        assert_eq!(fixed.palette, "violet");
        assert!(fixed.show_hidden); // untouched fields survive
        assert_eq!(fixed.terminal, "wt"); // untouched fields survive
        assert_eq!(fixed.density, "compact"); // invalid → field default
        assert_eq!(fixed.icon_style, "colored");
        assert_eq!(fixed.icon_theme, "Tile"); // FER-478 invalid → default
        assert_eq!(fixed.ui_font_level, "m"); // 99 unmapped → default
        assert!(fixed.show_extensions); // valid values survive
        assert!(fixed.folders_first);

        fs::write(
            dir.join(FILE_NAME),
            r#"{"theme":"dark","palette":"violet","showHidden":false,"terminal":"bogus"}"#,
        )
        .unwrap();
        let legacy = load_from(&dir);
        assert_eq!(legacy.terminal, "auto");
        // Fields absent from a pre-Appearance config fall back to defaults.
        assert_eq!(legacy.density, "compact");
        assert_eq!(legacy.icon_style, "colored");
        assert_eq!(legacy.icon_theme, "Tile");
        assert_eq!(legacy.ui_font_level, "m");
        assert!(legacy.show_extensions);
        assert!(legacy.folders_first);

        fs::write(dir.join(FILE_NAME), "not json at all {{{").unwrap();
        assert_eq!(load_from(&dir), Prefs::default());

        // FER-406: out-of-range, non-finite and missing paneSplit reset to 0.5
        // while the other fields survive untouched.
        fs::write(
            dir.join(FILE_NAME),
            r#"{"theme":"dark","palette":"violet","showHidden":true,"terminal":"wt","paneSplit":0.95}"#,
        )
        .unwrap();
        let fixed = load_from(&dir);
        assert_eq!(fixed.pane_split, 0.5);
        assert!(fixed.show_hidden);

        // JSON cannot carry NaN; the non-finite rule is covered directly.
        assert_eq!(normalize_pane_split(f64::NAN), 0.5);
        assert_eq!(normalize_pane_split(f64::INFINITY), 0.5);
        assert_eq!(normalize_pane_split(0.0), 0.5);
        assert_eq!(normalize_pane_split(0.2), 0.2); // bounds themselves are valid
        assert_eq!(normalize_pane_split(0.8), 0.8);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_three_palette_ids_stay_valid() {
        let dir = temp("legacy");
        for id in ["violet", "electric", "aqua"] {
            fs::write(
                dir.join(FILE_NAME),
                format!(
                    r#"{{"theme":"dark","palette":"{id}","showHidden":false,"terminal":"auto"}}"#
                ),
            )
            .unwrap();
            assert_eq!(load_from(&dir).palette, id);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_numeric_font_size_migrates_to_level() {
        let dir = temp("fontmig");
        let cases = [(12u64, "s"), (13, "m"), (14, "l")];
        for (size, level) in cases {
            fs::write(
                dir.join(FILE_NAME),
                format!(
                    r#"{{"theme":"dark","palette":"violet","showHidden":false,"terminal":"auto","uiFontSize":{size}}}"#
                ),
            )
            .unwrap();
            assert_eq!(load_from(&dir).ui_font_level, level);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn save_normalizes_before_writing() {
        let dir = temp("normalize");
        save_to(
            &dir,
            &Prefs {
                theme: "bogus".into(),
                palette: "bogus".into(),
                show_hidden: false,
                terminal: "bogus".into(),
                pane_split: 9.0,
                tabs: TabsPrefs {
                    open_next_to_current: false,
                    restore_tabs: false,
                    tab_width: "bogus".into(),
                    close_on_middle_click: false,
                },
                session: Some(SessionSnapshot {
                    panes: vec![PaneSession {
                        tabs: vec!["".into()],
                        current: 0,
                    }],
                }),
                preview: PreviewPrefs {
                    max_text_bytes: 7, // invalid → default
                    ..PreviewPrefs::default()
                },
                density: "bogus".into(),
                icon_style: "bogus".into(),
                icon_theme: "bogus".into(),
                ui_font_level: "bogus".into(),
                // Valid booleans are not normalized, so they must equal the
                // defaults for the roundtrip to equal `Prefs::default()`.
                show_extensions: true,
                folders_first: true,
                disk_usage_fast_offer: true,
                disk_usage_cache_warn_hours: 0, // invalid -> default 24
                address_own_row: false,
                indexer_folder_sizes: false,
                sync_journal_skip: false,
                confirm_before_deleting: false,
                verify_after_copy: false,
                copy_buffer_kib: 0,
                copy_threads: 99,
                copy_safe_write: false,
                copy_priority: "bogus".into(),
                conflict_policy: "bogus".into(),
                preserve_timestamps: false,
                preserve_permissions: false,
                rar_exe_path: None,
            },
        )
        .unwrap();
        let loaded = load_from(&dir);
        assert_eq!(loaded.theme, "dark");
        assert_eq!(loaded.density, "compact");
        assert_eq!(loaded.ui_font_level, "m");
        assert_eq!(loaded.copy_buffer_kib, COPY_BUFFER_MIN_KIB);
        assert_eq!(
            loaded.copy_threads, COPY_THREADS_MAX,
            "FER-471: out-of-range clamps"
        );
        assert_eq!(loaded.copy_priority, "normal");
        assert_eq!(loaded.disk_usage_cache_warn_hours, 24);
        assert_eq!(loaded.conflict_policy, "ask");
        assert_eq!(loaded.tabs.tab_width, "fit");
        assert_eq!(loaded.session, None); // nothing restorable → dropped whole
        let _ = fs::remove_dir_all(&dir);
    }

    // ── FER-395: tabs section + session snapshot ────────────────────────────

    #[test]
    fn missing_tabs_and_session_take_defaults() {
        let dir = temp("tabs-default");
        // Old config without the tabs/session fields: serde defaults fill in,
        // saved user choices (none here) are never overwritten.
        fs::write(
            dir.join(FILE_NAME),
            r#"{"theme":"dark","palette":"violet","showHidden":true,"terminal":"auto","paneSplit":0.5}"#,
        )
        .unwrap();
        let p = load_from(&dir);
        assert_eq!(p.tabs, TabsPrefs::default());
        assert_eq!(p.session, None);
        assert!(p.show_hidden); // untouched fields survive
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn tabs_round_trip_and_tab_width_validation() {
        let dir = temp("tabs-round");
        let p = Prefs {
            tabs: TabsPrefs {
                open_next_to_current: false,
                restore_tabs: true,
                tab_width: "fixed".into(),
                close_on_middle_click: false,
            },
            ..Prefs::default()
        };
        save_to(&dir, &p).unwrap();
        assert_eq!(load_from(&dir).tabs, p.tabs);

        fs::write(
            dir.join(FILE_NAME),
            r#"{"theme":"dark","palette":"violet","showHidden":false,"terminal":"auto","paneSplit":0.5,"tabs":{"openNextToCurrent":true,"restoreTabs":true,"tabWidth":"wide","closeOnMiddleClick":false}}"#,
        )
        .unwrap();
        let tabs = load_from(&dir).tabs;
        assert_eq!(tabs.tab_width, "fit"); // invalid width → default
        assert!(!tabs.close_on_middle_click); // untouched fields survive
        let _ = fs::remove_dir_all(&dir);
    }

    // ── FER-396: preview section ────────────────────────────────────────────

    #[test]
    fn preview_defaults_match_contract() {
        let d = PreviewPrefs::default();
        assert!(!d.open_on_launch);
        assert_eq!(d.max_text_bytes, 2_000_000);
        assert!(d.syntax_highlight);
        assert!(!d.autoplay_video);
        assert!(d.read_metadata);
        assert_eq!(Prefs::default().preview, d);
    }

    #[test]
    fn old_config_without_preview_loads_defaults_and_keeps_prefs() {
        let dir = temp("preview-migrate");
        fs::write(
            dir.join(FILE_NAME),
            r#"{"theme":"light","palette":"aqua","showHidden":true,"terminal":"wt","paneSplit":0.6,"tabs":{"openNextToCurrent":false,"restoreTabs":true,"tabWidth":"fixed","closeOnMiddleClick":true}}"#,
        )
        .unwrap();
        let p = load_from(&dir);
        assert_eq!(p.preview, PreviewPrefs::default());
        assert_eq!(p.theme, "light");
        assert_eq!(p.palette, "aqua");
        assert!(p.show_hidden);
        assert_eq!(p.tabs.tab_width, "fixed");
        assert!((p.pane_split - 0.6).abs() < 1e-9);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn preview_invalid_max_text_bytes_resets_only_that_field() {
        let dir = temp("preview-invalid");
        fs::write(
            dir.join(FILE_NAME),
            r#"{"preview":{"openOnLaunch":true,"maxTextBytes":123,"syntaxHighlight":false,"autoplayVideo":true,"readMetadata":false}}"#,
        )
        .unwrap();
        let pv = load_from(&dir).preview;
        assert_eq!(pv.max_text_bytes, 2_000_000);
        assert!(pv.open_on_launch);
        assert!(!pv.syntax_highlight);
        assert!(pv.autoplay_video);
        assert!(!pv.read_metadata);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn preview_mistyped_max_text_bytes_keeps_the_other_prefs() {
        for bad in [
            "\"4000000\"",
            "-5",
            "2000000.5",
            "null",
            "99999999999999999999999",
            "[1]",
            "true",
        ] {
            let dir = temp("preview-mistyped");
            fs::write(
                dir.join(FILE_NAME),
                format!(
                    r#"{{"theme":"light","palette":"aqua","preview":{{"openOnLaunch":true,"maxTextBytes":{bad},"autoplayVideo":true}}}}"#
                ),
            )
            .unwrap();
            let p = load_from(&dir);
            assert_eq!(p.preview.max_text_bytes, 2_000_000, "{bad}");
            assert!(p.preview.open_on_launch, "{bad}");
            assert!(p.preview.autoplay_video, "{bad}");
            assert_eq!(p.theme, "light", "{bad}");
            assert_eq!(p.palette, "aqua", "{bad}");
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn preview_not_an_object_keeps_the_other_prefs() {
        let dir = temp("preview-notobj");
        fs::write(dir.join(FILE_NAME), r#"{"theme":"light","preview":"oops"}"#).unwrap();
        let p = load_from(&dir);
        assert_eq!(p.preview, PreviewPrefs::default());
        assert_eq!(p.theme, "light");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn preview_valid_max_text_bytes_is_kept_for_every_limit() {
        for limit in PREVIEW_TEXT_LIMITS {
            let dir = temp("preview-valid");
            fs::write(
                dir.join(FILE_NAME),
                format!(r#"{{"preview":{{"maxTextBytes":{limit}}}}}"#),
            )
            .unwrap();
            assert_eq!(load_from(&dir).preview.max_text_bytes, limit);
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn preview_mistyped_bool_fields_reset_only_that_field() {
        for field in [
            "openOnLaunch",
            "syntaxHighlight",
            "autoplayVideo",
            "readMetadata",
        ] {
            for bad in ["\"yes\"", "1", "null", "[]", "{}"] {
                let dir = temp("preview-badbool");
                // Every other field carries a NON-default value so a wipe shows.
                let mut pv = serde_json::json!({
                    "openOnLaunch": true,
                    "maxTextBytes": 500000,
                    "syntaxHighlight": false,
                    "autoplayVideo": true,
                    "readMetadata": false
                });
                pv[field] = serde_json::from_str(bad).unwrap();
                let raw = serde_json::json!({"theme": "light", "preview": pv});
                fs::write(dir.join(FILE_NAME), raw.to_string()).unwrap();
                let p = load_from(&dir);
                let d = PreviewPrefs::default();
                let got = &p.preview;
                assert_eq!(p.theme, "light", "{field}={bad}");
                assert_eq!(
                    got.max_text_bytes,
                    if field == "maxTextBytes" {
                        d.max_text_bytes
                    } else {
                        500_000
                    }
                );
                let expect =
                    |name: &str, set: bool, def: bool| if field == name { def } else { set };
                assert_eq!(
                    got.open_on_launch,
                    expect("openOnLaunch", true, d.open_on_launch),
                    "{field}={bad}"
                );
                assert_eq!(
                    got.syntax_highlight,
                    expect("syntaxHighlight", false, d.syntax_highlight),
                    "{field}={bad}"
                );
                assert_eq!(
                    got.autoplay_video,
                    expect("autoplayVideo", true, d.autoplay_video),
                    "{field}={bad}"
                );
                assert_eq!(
                    got.read_metadata,
                    expect("readMetadata", false, d.read_metadata),
                    "{field}={bad}"
                );
                let _ = fs::remove_dir_all(&dir);
            }
        }
    }

    #[test]
    fn preview_unknown_fields_and_valid_values_survive() {
        let dir = temp("preview-unknown");
        fs::write(
            dir.join(FILE_NAME),
            r#"{"theme":"light","preview":{"futureField":{"x":1},"openOnLaunch":true,"maxTextBytes":100000}}"#,
        )
        .unwrap();
        let p = load_from(&dir);
        assert_eq!(p.theme, "light");
        assert!(p.preview.open_on_launch);
        assert_eq!(p.preview.max_text_bytes, 100_000);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn preview_round_trip_all_limits() {
        let dir = temp("preview-round");
        for limit in PREVIEW_TEXT_LIMITS {
            let p = Prefs {
                preview: PreviewPrefs {
                    open_on_launch: true,
                    max_text_bytes: limit,
                    syntax_highlight: false,
                    autoplay_video: true,
                    read_metadata: false,
                },
                ..Prefs::default()
            };
            save_to(&dir, &p).unwrap();
            assert_eq!(load_from(&dir).preview, p.preview);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn session_sanitizes_garbage() {
        let dir = temp("session-sanitize");
        // Over-limit panes/tabs are capped, empty paths drop, an
        // out-of-range current clamps to the first tab.
        fs::write(
            dir.join(FILE_NAME),
            r#"{"theme":"dark","palette":"violet","showHidden":false,"terminal":"auto","paneSplit":0.5,"session":{"panes":[
                {"tabs":["C:\\keep","","C:\\drop"],"current":99},
                {"tabs":[],"current":0},
                {"tabs":["C:\\overflow"],"current":0}
            ]}}"#,
        )
        .unwrap();
        let session = load_from(&dir).session.expect("a restorable pane survives");
        assert_eq!(session.panes.len(), 1); // the empty pane is skipped
        assert_eq!(
            session.panes[0].tabs,
            vec!["C:\\keep".to_string(), "C:\\drop".to_string()]
        );
        assert_eq!(session.panes[0].current, 0);

        // Over the tab cap the extra paths are cut (12 kept, nothing crashes).
        let mut many = vec!["C:\\x".to_string(); MAX_SESSION_TABS + 5];
        many[0] = "C:\\first".into();
        let fixed = sanitize_session(Some(SessionSnapshot {
            panes: vec![PaneSession {
                tabs: many,
                current: 0,
            }],
        }))
        .unwrap();
        assert_eq!(fixed.panes[0].tabs.len(), MAX_SESSION_TABS);
        assert_eq!(fixed.panes[0].tabs[0], "C:\\first");

        // JSON-level garbage still falls back to the whole-file default.
        fs::write(
            dir.join(FILE_NAME),
            r#"{"theme":"dark","session":"not-a-snapshot"}"#,
        )
        .unwrap();
        assert_eq!(load_from(&dir), Prefs::default());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn copy_buffer_uses_the_frontend_json_key() {
        let dir = temp("bufkey");
        fs::write(
            dir.join(FILE_NAME),
            r#"{"theme":"dark","palette":"violet","showHidden":false,"terminal":"auto","copyBufferKiB":4096}"#,
        )
        .unwrap();
        assert_eq!(load_from(&dir).copy_buffer_kib, 4096);
        // The key the first build wrote is still read.
        fs::write(dir.join(FILE_NAME), r#"{"copyBufferKib":2048}"#).unwrap();
        assert_eq!(load_from(&dir).copy_buffer_kib, 2048);
        save_to(
            &dir,
            &Prefs {
                copy_buffer_kib: 512,
                ..Prefs::default()
            },
        )
        .unwrap();
        let raw = fs::read_to_string(dir.join(FILE_NAME)).unwrap();
        assert!(raw.contains("\"copyBufferKiB\": 512"), "{raw}");
        assert!(!raw.contains("copyBufferKib"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn legacy_config_migrates_with_safe_file_operation_defaults() {
        let dir = temp("legacy-fileops");
        fs::write(
            dir.join(FILE_NAME),
            r#"{"theme":"light","palette":"aqua","showHidden":true,"terminal":"wt"}"#,
        )
        .unwrap();
        let p = load_from(&dir);
        assert_eq!(p.theme, "light");
        assert!(p.show_hidden);
        assert!(p.confirm_before_deleting);
        assert!(!p.verify_after_copy);
        assert_eq!(p.copy_buffer_kib, 1024);
        assert_eq!(p.conflict_policy, "ask");
        assert!(p.preserve_timestamps);
        assert!(!p.preserve_permissions);
        assert_eq!(p.rar_exe_path, None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rar_exe_path_defaults_none_and_survives_save_load() {
        let dir = temp("rarexepath");
        let def = Prefs::default();
        assert_eq!(def.rar_exe_path, None);

        // Loads as None if absent in json
        fs::write(dir.join(FILE_NAME), r#"{"theme":"dark"}"#).unwrap();
        assert_eq!(load_from(&dir).rar_exe_path, None);

        // Saves and loads correctly
        let p = Prefs {
            rar_exe_path: Some(r"C:\Program Files\WinRAR\Rar.exe".into()),
            ..Prefs::default()
        };
        save_to(&dir, &p).unwrap();
        assert_eq!(
            load_from(&dir).rar_exe_path,
            Some(r"C:\Program Files\WinRAR\Rar.exe".into())
        );

        // Normalizes empty string to None
        let mut p_empty = Prefs {
            rar_exe_path: Some("   ".into()),
            ..Prefs::default()
        };
        p_empty.normalize();
        assert_eq!(p_empty.rar_exe_path, None);

        let _ = fs::remove_dir_all(&dir);
    }
}
