//! Profile filters and glob matcher (SP-4.2, SP-4.3, SP-4.4; design §5.3).
//!
//! Excluded entries are invisible to the comparison on both sides: never copied,
//! never overwritten, never deleted. Excluded folders are never descended.
//!
//! Built-in excludes: copy staging and backup files (`.*.ferrum-stage-*`,
//! `.*.ferrum-backup-*`), `$RECYCLE.BIN`, `System Volume Information`.

use crate::compare::EntryFilter;
use serde::{Deserialize, Serialize};

/// Profile filter patterns (include and exclude).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Filters {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

/// Parsed single pattern for path matching.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pattern {
    raw: String,
    dir_only: bool,
    anchored: bool,
    segments: Vec<String>,
}

impl Pattern {
    /// Parse a filter pattern string.
    ///
    /// Syntax:
    /// - `*` matches 0 or more characters within one path component.
    /// - `?` matches 1 character within one path component.
    /// - `**` matches 0 or more directory components.
    /// - Trailing `\` matches directories only.
    /// - Patterns containing `\` (or starting with `\`) are anchored to root.
    /// - Patterns without `\` match either the basename or the full relative path.
    pub fn parse(raw: &str) -> Self {
        let trimmed = raw.trim();
        let (pattern_no_dir, dir_only) = if trimmed.ends_with('\\') || trimmed.ends_with('/') {
            (&trimmed[..trimmed.len() - 1], true)
        } else {
            (trimmed, false)
        };

        let (cleaned, anchored) =
            if pattern_no_dir.starts_with('\\') || pattern_no_dir.starts_with('/') {
                (&pattern_no_dir[1..], true)
            } else if pattern_no_dir.contains('\\') || pattern_no_dir.contains('/') {
                (pattern_no_dir, true)
            } else {
                (pattern_no_dir, false)
            };

        let segments: Vec<String> = cleaned
            .split(['\\', '/'])
            .filter(|s| !s.is_empty())
            .map(|s| s.to_lowercase())
            .collect();

        Self {
            raw: raw.to_string(),
            dir_only,
            anchored,
            segments,
        }
    }

    /// True if the pattern matches the path and kind.
    pub fn matches(&self, rel_path: &str, is_dir: bool) -> bool {
        if self.dir_only && !is_dir {
            return false;
        }

        let path_segments: Vec<String> = rel_path
            .split(['\\', '/'])
            .filter(|s| !s.is_empty())
            .map(|s| s.to_lowercase())
            .collect();

        let seg_refs: Vec<&str> = self.segments.iter().map(|s| s.as_str()).collect();
        let path_refs: Vec<&str> = path_segments.iter().map(|s| s.as_str()).collect();

        if self.anchored {
            match_path_segments(&seg_refs, &path_refs)
        } else {
            // Unanchored pattern (no slash): matches full path OR basename
            match_path_segments(&seg_refs, &path_refs)
                || (!path_refs.is_empty()
                    && match_path_segments(&seg_refs, &path_refs[path_refs.len() - 1..]))
        }
    }

    /// For include checking: could a directory lead to entries matching this pattern?
    pub fn could_contain(&self, dir_segments: &[&str]) -> bool {
        if !self.anchored {
            // An unanchored pattern (e.g. *.txt) can match at any depth in any directory
            return true;
        }

        let seg_refs: Vec<&str> = self.segments.iter().map(|s| s.as_str()).collect();
        prefix_could_lead(&seg_refs, dir_segments)
    }
}

fn prefix_could_lead(pattern_segs: &[&str], dir_segs: &[&str]) -> bool {
    if dir_segs.is_empty() {
        return true;
    }
    if pattern_segs.is_empty() {
        return false;
    }
    if pattern_segs[0] == "**" {
        return true;
    }
    if match_segment_str(pattern_segs[0], dir_segs[0]) {
        return prefix_could_lead(&pattern_segs[1..], &dir_segs[1..]);
    }
    false
}

/// Match path segments where `**` matches 0 or more segments.
fn match_path_segments(pattern_segs: &[&str], path_segs: &[&str]) -> bool {
    if pattern_segs.is_empty() {
        return path_segs.is_empty();
    }
    if pattern_segs[0] == "**" {
        // ** matches 0 segments, or 1 segment and continues
        return match_path_segments(&pattern_segs[1..], path_segs)
            || (!path_segs.is_empty() && match_path_segments(pattern_segs, &path_segs[1..]));
    }
    if path_segs.is_empty() {
        return false;
    }
    if match_segment_str(pattern_segs[0], path_segs[0]) {
        return match_path_segments(&pattern_segs[1..], &path_segs[1..]);
    }
    false
}

/// Match a single path component using `*` (0+ chars) and `?` (1 char).
fn match_segment_str(pattern: &str, text: &str) -> bool {
    let p_chars: Vec<char> = pattern.chars().collect();
    let t_chars: Vec<char> = text.chars().collect();

    let mut p_idx = 0;
    let mut t_idx = 0;
    let mut star_p = None;
    let mut match_t = 0;

    while t_idx < t_chars.len() {
        if p_idx < p_chars.len() && (p_chars[p_idx] == '?' || p_chars[p_idx] == t_chars[t_idx]) {
            p_idx += 1;
            t_idx += 1;
        } else if p_idx < p_chars.len() && p_chars[p_idx] == '*' {
            star_p = Some(p_idx);
            p_idx += 1;
            match_t = t_idx;
        } else if let Some(sp) = star_p {
            p_idx = sp + 1;
            match_t += 1;
            t_idx = match_t;
        } else {
            return false;
        }
    }

    while p_idx < p_chars.len() && p_chars[p_idx] == '*' {
        p_idx += 1;
    }

    p_idx == p_chars.len()
}

/// Check if a relative path matches built-in non-editable excludes (SP-4.4).
pub fn is_builtin_exclude(rel: &str) -> bool {
    // Staging and backup files from copy_engine: .*.ferrum-stage-*, .*.ferrum-backup-*
    for comp in rel.split(['\\', '/']) {
        if comp.starts_with('.')
            && (comp.contains(".ferrum-stage-") || comp.contains(".ferrum-backup-"))
        {
            return true;
        }
    }

    // At volume root: $RECYCLE.BIN and System Volume Information
    if rel.split(['\\', '/']).next().is_some_and(|first| {
        first.eq_ignore_ascii_case("$RECYCLE.BIN")
            || first.eq_ignore_ascii_case("System Volume Information")
    }) {
        return true;
    }

    false
}

/// Runtime filter implementing [`EntryFilter`].
#[derive(Debug, Clone)]
pub struct ProfileFilter {
    include: Vec<Pattern>,
    exclude: Vec<Pattern>,
}

impl ProfileFilter {
    pub fn new(include: &[String], exclude: &[String]) -> Self {
        Self {
            include: include
                .iter()
                .filter(|s| !s.trim().is_empty())
                .map(|s| Pattern::parse(s))
                .collect(),
            exclude: exclude
                .iter()
                .filter(|s| !s.trim().is_empty())
                .map(|s| Pattern::parse(s))
                .collect(),
        }
    }

    pub fn from_filters(filters: &Filters) -> Self {
        Self::new(&filters.include, &filters.exclude)
    }

    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }
}

impl EntryFilter for ProfileFilter {
    fn visible(&self, rel: &str, is_dir: bool) -> bool {
        // Built-in excludes always apply
        if is_builtin_exclude(rel) {
            return false;
        }

        // Exclude list
        for p in &self.exclude {
            if p.matches(rel, is_dir) {
                return false;
            }
        }

        // Include list
        if self.include.is_empty() {
            return true;
        }

        if !is_dir {
            self.include.iter().any(|p| p.matches(rel, false))
        } else {
            let dir_segments: Vec<String> = rel
                .split(['\\', '/'])
                .filter(|s| !s.is_empty())
                .map(|s| s.to_lowercase())
                .collect();
            let seg_refs: Vec<&str> = dir_segments.iter().map(|s| s.as_str()).collect();

            self.include
                .iter()
                .any(|p| p.matches(rel, true) || p.could_contain(&seg_refs))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_excludes() {
        // Staging and backup files
        assert!(is_builtin_exclude(".file.txt.ferrum-stage-1234-1"));
        assert!(is_builtin_exclude(r"sub\dir\.file.txt.ferrum-stage-1234-1"));
        assert!(is_builtin_exclude(".data.ferrum-backup-5678-0"));
        assert!(!is_builtin_exclude("normal.txt"));

        // Volume root special folders
        assert!(is_builtin_exclude("$RECYCLE.BIN"));
        assert!(is_builtin_exclude(r"$recycle.bin\sub"));
        assert!(is_builtin_exclude("System Volume Information"));
        assert!(is_builtin_exclude(r"system volume information\catalog"));
        assert!(!is_builtin_exclude("My Documents"));
    }

    #[test]
    fn test_glob_wildcards() {
        let p_star = Pattern::parse("*.tmp");
        assert!(p_star.matches("test.tmp", false));
        assert!(p_star.matches(r"sub\test.tmp", false));
        assert!(!p_star.matches("test.txt", false));

        let p_question = Pattern::parse("file?.txt");
        assert!(p_question.matches("file1.txt", false));
        assert!(p_question.matches("fileA.txt", false));
        assert!(!p_question.matches("file12.txt", false));

        let p_globstar = Pattern::parse(r"docs\**\*.md");
        assert!(p_globstar.matches(r"docs\readme.md", false));
        assert!(p_globstar.matches(r"docs\sub\section\page.md", false));
        assert!(!p_globstar.matches(r"src\readme.md", false));
    }

    #[test]
    fn test_trailing_slash_folders_only() {
        let p_dir = Pattern::parse(r"node_modules\");
        assert!(p_dir.matches("node_modules", true));
        assert!(p_dir.matches(r"project\node_modules", true));
        assert!(!p_dir.matches("node_modules", false)); // not a dir
        assert!(!p_dir.matches(r"project\node_modules", false));
    }

    #[test]
    fn test_case_insensitivity_non_ascii() {
        let p = Pattern::parse(r"Über\*.TXT");
        assert!(p.matches(r"über\notes.txt", false));
        assert!(p.matches(r"ÜBER\DOC.TXT", false));
        assert!(!p.matches(r"other\notes.txt", false));
    }

    #[test]
    fn test_include_exclude_interplay() {
        let filter = ProfileFilter::new(
            &["*.txt".to_string(), r"docs\**".to_string()],
            &["secret.txt".to_string(), r"docs\private\**".to_string()],
        );

        // Files
        assert!(filter.visible("readme.txt", false));
        assert!(!filter.visible("secret.txt", false)); // excluded
        assert!(!filter.visible("image.png", false)); // not included
        assert!(filter.visible(r"docs\guide.html", false)); // included by docs\**
        assert!(!filter.visible(r"docs\private\secret.html", false)); // excluded by docs\private\**

        // Folders
        assert!(filter.visible("sub", true)); // can contain *.txt
        assert!(filter.visible("docs", true)); // matches docs\**
        assert!(!filter.visible(r"docs\private", true)); // excluded
    }
}
