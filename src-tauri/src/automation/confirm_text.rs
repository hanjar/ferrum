//! Native confirmation prompt texts (Automation design §10.3).
//!
//! Order: warning lines → action line → profile line → (later packages: trigger, runtime) →
//! closing line. Every part is built by the backend from validated, stored data (never
//! webview-supplied display text, SE-2/threat T4), shortened to [`PART_MAX_CHARS`] characters
//! with an ellipsis in the middle, and the whole text stays within [`PROMPT_MAX_CHARS`].

use crate::automation::model::{ProfilePolicy, SyncMode, delete_label, mode_label, shorten_middle};
use crate::hotlist::model::is_format_char;

/// One part of a prompt is at most this many characters (§10.3).
pub const PART_MAX_CHARS: usize = 120;
/// Upper bound for the whole dialog text (§10.3, SE-2).
pub const PROMPT_MAX_CHARS: usize = 2000;

/// Caption of the profile-save confirmation (the message box itself comes from
/// `native_dialog::confirm`).
pub const SAVE_CAPTION: &str = "Ferrum - save sync profile?";

pub const PERMANENT_WARNING: &str =
    "WARNING: deleted files are removed permanently, without the Recycle Bin.";
pub const GUARD_OFF_WARNING: &str =
    "WARNING: the delete safety guard is off. A run could delete many files without stopping.";
pub const PROPAGATE_WARNING: &str = "WARNING: two-way deletion propagation can delete files on either side when the stored baseline proves them changed.";

const CLOSING: &str = "Ferrum asks again whenever the profile's folders, direction, delete policy, filters or guard change.";

/// What a caller shows and answers: the caption plus the fully built text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    pub caption: &'static str,
    pub text: String,
}

/// One bounded part of the text. Every part is already validated (no control or format
/// characters get past profile validation); as defence in depth any such character that still
/// arrives is shown as a visible `<U+XXXX>` marker, so nothing can reorder or hide the text.
fn part(s: &str) -> String {
    let visible: String = s
        .chars()
        .map(|c| {
            if c.is_control() || is_format_char(c) {
                format!("<U+{:04X}>", c as u32)
            } else {
                c.to_string()
            }
        })
        .collect();
    shorten_middle(&visible, PART_MAX_CHARS)
}

/// Prompt for saving a profile whose delete policy is `permanent` (new, or changed to) and/or
/// whose delete guard is turned off (SP-3.3, SP-5.1, SE-1). The text starts with the SE-2.2
/// warning when the policy deletes permanently.
pub fn save_profile_prompt(
    name: &str,
    current_revision: Option<i64>,
    policy: &ProfilePolicy,
) -> Prompt {
    let mut lines: Vec<String> = Vec::new();
    if matches!(
        policy.delete_policy,
        crate::automation::model::DeletePolicy::Permanent
    ) {
        lines.push(PERMANENT_WARNING.into());
    }
    if !policy.guard.enabled {
        lines.push(GUARD_OFF_WARNING.into());
    }
    if matches!(
        policy.mode,
        SyncMode::TwoWay {
            propagate_deletes: true
        }
    ) {
        lines.push(PROPAGATE_WARNING.into());
    }
    let action = match current_revision {
        Some(rev) => format!("Save sync profile \"{}\" (now rev {rev})?", part(name)),
        None => format!("Save sync profile \"{}\"?", part(name)),
    };
    lines.push(action);
    lines.push(format!(
        "{} · {} → {} · {} · {}",
        part(&mode_label(&policy.mode)),
        part(&policy.left.path),
        part(&policy.right.path),
        part(&delete_label(&policy.delete_policy)),
        if policy.guard.enabled {
            "guard on".to_string()
        } else {
            "guard off".to_string()
        },
    ));
    lines.push(CLOSING.into());
    Prompt {
        caption: SAVE_CAPTION,
        text: lines.join("\n\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::automation::model::{
        CompareMode, DeleteGuard, DeletePolicy, Filters, RootIdentity, RootSpec, Side, SyncMode,
    };

    fn policy(delete: DeletePolicy, guard_on: bool) -> ProfilePolicy {
        let root = |p: &str| RootSpec {
            path: p.into(),
            identity: RootIdentity::Local {
                volume_serial: "0123abcd".into(),
                file_id: "f".repeat(32),
                fs_name: "NTFS".into(),
                drive: crate::automation::model::DriveKind::Fixed,
            },
        };
        ProfilePolicy {
            left: root(r"C:\A"),
            right: root(r"D:\B"),
            mode: SyncMode::OneWay { from: Side::Left },
            delete_policy: delete,
            compare: CompareMode::SizeAndDate,
            filters: Filters {
                include: vec![],
                exclude: vec![],
            },
            guard: DeleteGuard {
                enabled: guard_on,
                max_count: 50,
                max_percent: 20,
            },
        }
    }

    /// SE-2.2: the permanent-delete text starts with the exact warning.
    #[test]
    fn se_2_2_the_permanent_delete_text_starts_with_the_warning() {
        let p = save_profile_prompt("Nightly", Some(3), &policy(DeletePolicy::Permanent, true));
        assert!(p.text.starts_with(PERMANENT_WARNING), "{}", p.text);
        assert_eq!(p.caption, SAVE_CAPTION);
        assert!(
            p.text
                .contains("Save sync profile \"Nightly\" (now rev 3)?")
        );
        assert!(p.text.contains(r"C:\A"));
        assert!(p.text.contains(r"D:\B"));
        assert!(p.text.contains("Permanent"));
    }

    /// SP-5.1: turning the guard off is confirmed too, with its own warning line before the
    /// action line.
    #[test]
    fn guard_off_gets_its_own_warning_before_the_action() {
        let p = save_profile_prompt("N", None, &policy(DeletePolicy::RecycleBin, false));
        let warning_at = p.text.find(GUARD_OFF_WARNING).unwrap();
        let action_at = p.text.find("Save sync profile").unwrap();
        assert!(warning_at < action_at);
        assert!(!p.text.contains(PERMANENT_WARNING));
    }

    /// §10.3: every part is bounded and the total text never exceeds 2000 characters, even for
    /// pathological names and paths.
    #[test]
    fn the_text_stays_bounded_and_orders_warning_action_profile_closing() {
        let long = "x".repeat(4000);
        let mut p = policy(DeletePolicy::Permanent, false);
        p.mode = SyncMode::TwoWay {
            propagate_deletes: true,
        };
        p.right.path = long.clone();
        let prompt = save_profile_prompt(&long, Some(12), &p);
        assert!(prompt.text.starts_with(PERMANENT_WARNING));
        assert!(prompt.text.contains(GUARD_OFF_WARNING));
        assert!(prompt.text.contains(PROPAGATE_WARNING));
        let closing_at = prompt.text.find(CLOSING).unwrap();
        let action_at = prompt.text.find("Save sync profile").unwrap();
        assert!(prompt.text.find(GUARD_OFF_WARNING).unwrap() < action_at);
        assert!(action_at < closing_at);
        let len = prompt.text.chars().count();
        assert!(len <= PROMPT_MAX_CHARS, "{len} characters");
        // The shortened root shows head and tail with an ellipsis in the middle, never the whole
        // 4000 characters.
        assert!(!prompt.text.contains(&long), "{}", prompt.text);
        assert!(
            prompt
                .text
                .contains(&format!("{}…{}", "x".repeat(60), "x".repeat(59)))
        );
    }

    /// A save that needs no confirmation (recycle bin, guard on) has no warnings at all — its
    /// prompt is only built when `confirm_needed` said so.
    #[test]
    fn a_plain_policy_would_only_name_the_action() {
        let p = save_profile_prompt("N", None, &policy(DeletePolicy::RecycleBin, true));
        assert!(p.text.starts_with("Save sync profile"));
        assert!(!p.text.contains("WARNING"));
    }

    /// P2-3 (defence in depth): even if a control or bidi character reached the prompt builder,
    /// it is shown as a visible marker instead of reordering the text.
    #[test]
    fn control_and_bidi_characters_are_made_visible_never_passed_through() {
        let mut p = policy(DeletePolicy::Permanent, true);
        p.right.path = "D:\\B\u{202E}gpj.exe".into();
        let prompt = save_profile_prompt("Night\u{200E}ly\u{7}", Some(2), &p);
        assert!(!prompt.text.contains('\u{202E}'));
        assert!(!prompt.text.contains('\u{200E}'));
        assert!(!prompt.text.contains('\u{7}'));
        assert!(prompt.text.contains("<U+202E>"), "{}", prompt.text);
        assert!(prompt.text.contains("Night<U+200E>ly<U+0007>"));
    }
}
