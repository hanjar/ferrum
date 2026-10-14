//! Profile filters and glob matcher (SP-4.1, SP-4.2, SP-4.3, SP-4.4; design §5.3).
//!
//! Excluded entries are invisible to the comparison on both sides: never copied,
//! never overwritten, never deleted. Excluded folders are never descended.
//!
//! Built-in excludes: copy staging and backup files (`.*.ferrum-stage-*`,
//! `.*.ferrum-backup-*`), `$RECYCLE.BIN`, `System Volume Information`.

use crate::compare::EntryFilter;
use serde::{Deserialize, Serialize};

/// Maximum allowed patterns per list (SP-4.2).
pub const MAX_PATTERNS_PER_LIST: usize = 64;
/// Maximum character length per pattern (SP-4.2).
pub const MAX_PATTERN_CHARS: usize = 260;

/// Profile filter patterns (include and exclude).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Filters {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,
}

/// Errors when validating filter pattern limits (SP-4.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterError {
    TooManyIncludePatterns(usize),
    TooManyExcludePatterns(usize),
    PatternTooLong { pattern: String, len: usize },
}

impl std::fmt::Display for FilterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooManyIncludePatterns(n) => {
                write!(
                    f,
                    "Too many include patterns ({n} > {MAX_PATTERNS_PER_LIST})"
                )
            }
            Self::TooManyExcludePatterns(n) => {
                write!(
                    f,
                    "Too many exclude patterns ({n} > {MAX_PATTERNS_PER_LIST})"
                )
            }
            Self::PatternTooLong { pattern, len } => {
                write!(
                    f,
                    "Filter pattern exceeds maximum length of {MAX_PATTERN_CHARS} characters ({len} chars): '{pattern}'"
                )
            }
        }
    }
}

impl std::error::Error for FilterError {}

/// Simple case folding per character (P3-2).
/// Uses `to_lowercase()` if and only if the mapping produces exactly one character;
/// otherwise preserves the character unchanged (e.g. Turkish `İ` remains `İ`).
pub fn fold_char(c: char) -> char {
    let mut it = c.to_lowercase();
    match (it.next(), it.next()) {
        (Some(single), None) => single,
        _ => c,
    }
}

pub fn fold_str(s: &str) -> String {
    s.chars().map(fold_char).collect()
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
    /// - `**` matches 0 or more directory components. Consecutive `**` are collapsed (P2-3).
    /// - Trailing `\` or `/` matches directories only.
    /// - Patterns containing `\` or `/` are anchored to the root.
    /// - Patterns without `\` or `/` match the basename at any depth (gitignore style, P3-1).
    /// - Separators `/` and `\` are normalized interchangeably (P3-1).
    pub fn parse(raw: &str) -> Self {
        let trimmed = raw.trim();
        let (pattern_no_dir, dir_only) = if trimmed.ends_with('\\') || trimmed.ends_with('/') {
            (trimmed.trim_end_matches(['\\', '/']), true)
        } else {
            (trimmed, false)
        };

        // P1-B (Rework 2): Trailing slash only indicates directories and does not anchor the pattern.
        // Anchoring is determined from pattern without trailing slash.
        // A pattern with internal slash (e.g. `build\out\`) or leading slash (e.g. `/node_modules`) is anchored.
        // A pattern without slash (e.g. `node_modules\`) is unanchored and matches at any depth.
        let anchored = pattern_no_dir.contains(['\\', '/'])
            || trimmed.starts_with('\\')
            || trimmed.starts_with('/');
        let cleaned = if pattern_no_dir.starts_with('\\') || pattern_no_dir.starts_with('/') {
            pattern_no_dir.trim_start_matches(['\\', '/'])
        } else {
            pattern_no_dir
        };

        let raw_segs: Vec<String> = cleaned
            .split(['\\', '/'])
            .filter(|s| !s.is_empty())
            .map(fold_str)
            .collect();

        // Collapse consecutive `**` segments (P2-3)
        let mut segments = Vec::with_capacity(raw_segs.len());
        for seg in raw_segs {
            if seg == "**" && segments.last().map(|s: &String| s.as_str()) == Some("**") {
                continue;
            }
            segments.push(seg);
        }

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
            .map(fold_str)
            .collect();

        let seg_refs: Vec<&str> = self.segments.iter().map(|s| s.as_str()).collect();
        let path_refs: Vec<&str> = path_segments.iter().map(|s| s.as_str()).collect();

        if self.anchored {
            match_path_segments(&seg_refs, &path_refs)
        } else {
            // Unanchored pattern (no slash): matches basename at any depth (gitignore style, P3-1)
            !path_refs.is_empty()
                && match_path_segments(&seg_refs, &path_refs[path_refs.len() - 1..])
        }
    }

    /// For include checking: could a directory lead to entries matching this pattern?
    pub fn could_contain(&self, dir_segments: &[&str]) -> bool {
        if self.dir_only {
            // For a dir_only pattern (e.g. `docs\` or `build\out\`), the directory itself
            // matches directly or via ancestor match. A non-matching directory (like `photos`)
            // does not match `docs\` and must not be made visible.
            if self.anchored {
                let seg_refs: Vec<&str> = self.segments.iter().map(|s| s.as_str()).collect();
                prefix_could_lead(&seg_refs, dir_segments)
            } else {
                // Unanchored folder-only pattern (`docs\`) matches at any depth, so
                // any directory may lead to a match.
                true
            }
        } else if !self.anchored {
            // An unanchored file pattern (e.g. *.txt) can match at any depth in any directory
            true
        } else {
            let seg_refs: Vec<&str> = self.segments.iter().map(|s| s.as_str()).collect();
            prefix_could_lead(&seg_refs, dir_segments)
        }
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

/// Match path segments using DP $O(P \cdot N)$ where `**` matches 0 or more segments (P2-3).
fn match_path_segments(pattern_segs: &[&str], path_segs: &[&str]) -> bool {
    let n_len = path_segs.len();

    let mut prev = vec![false; n_len + 1];
    let mut curr = vec![false; n_len + 1];
    prev[0] = true;

    for &pat in pattern_segs {
        curr.fill(false);
        if pat == "**" {
            curr[0] = prev[0];
            for j in 1..=n_len {
                curr[j] = prev[j] || curr[j - 1];
            }
        } else {
            for j in 1..=n_len {
                if prev[j - 1] && match_segment_str(pat, path_segs[j - 1]) {
                    curr[j] = true;
                }
            }
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[n_len]
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

    // At volume root (or first relative segment): $RECYCLE.BIN and System Volume Information
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
    /// Create a new profile filter enforcing SP-4.2 limits (<= 64 patterns, <= 260 chars).
    pub fn new(include: &[String], exclude: &[String]) -> Result<Self, FilterError> {
        let non_empty_inc: Vec<&String> = include.iter().filter(|s| !s.trim().is_empty()).collect();
        if non_empty_inc.len() > MAX_PATTERNS_PER_LIST {
            return Err(FilterError::TooManyIncludePatterns(non_empty_inc.len()));
        }
        let non_empty_exc: Vec<&String> = exclude.iter().filter(|s| !s.trim().is_empty()).collect();
        if non_empty_exc.len() > MAX_PATTERNS_PER_LIST {
            return Err(FilterError::TooManyExcludePatterns(non_empty_exc.len()));
        }

        for &p in non_empty_inc.iter().chain(non_empty_exc.iter()) {
            let char_count = p.chars().count();
            if char_count > MAX_PATTERN_CHARS {
                return Err(FilterError::PatternTooLong {
                    pattern: p.clone(),
                    len: char_count,
                });
            }
        }

        Ok(Self {
            include: non_empty_inc
                .into_iter()
                .map(|s| Pattern::parse(s))
                .collect(),
            exclude: non_empty_exc
                .into_iter()
                .map(|s| Pattern::parse(s))
                .collect(),
        })
    }

    pub fn from_filters(filters: &Filters) -> Result<Self, FilterError> {
        Self::new(&filters.include, &filters.exclude)
    }

    pub fn is_empty(&self) -> bool {
        self.include.is_empty() && self.exclude.is_empty()
    }
}

impl EntryFilter for ProfileFilter {
    fn visible(&self, rel: &str, is_dir: bool) -> bool {
        // Built-in excludes always apply (SP-4.4)
        if is_builtin_exclude(rel) {
            return false;
        }

        let segments: Vec<String> = rel
            .split(['\\', '/'])
            .filter(|s| !s.is_empty())
            .map(fold_str)
            .collect();

        // Exclude list (SP-4.3): direct match or ancestor directory match
        for p in &self.exclude {
            if p.matches(rel, is_dir) {
                return false;
            }
        }
        for prefix_len in 1..segments.len() {
            let ancestor_rel = segments[..prefix_len].join("/");
            for p in &self.exclude {
                if p.matches(&ancestor_rel, true) {
                    return false;
                }
            }
        }

        // Include list (SP-4.3)
        if self.include.is_empty() {
            return true;
        }

        // Direct match with include pattern
        if self.include.iter().any(|p| p.matches(rel, is_dir)) {
            return true;
        }

        // P2-6: include matching a folder recursively includes all its contents.
        // Check if any ancestor directory matches an include pattern (as a directory).
        for prefix_len in 1..segments.len() {
            let ancestor_rel = segments[..prefix_len].join("/");
            if self.include.iter().any(|p| p.matches(&ancestor_rel, true)) {
                return true;
            }
        }

        // For directories, also keep visible if an include pattern could match inside (descend).
        if is_dir {
            let seg_refs: Vec<&str> = segments.iter().map(|s| s.as_str()).collect();
            self.include.iter().any(|p| p.could_contain(&seg_refs))
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    #[test]
    fn test_builtin_excludes() {
        // SP-4.4: Staging and backup files
        assert!(is_builtin_exclude(".file.txt.ferrum-stage-1234-1"));
        assert!(is_builtin_exclude(r"sub\dir\.file.txt.ferrum-stage-1234-1"));
        assert!(is_builtin_exclude(".data.ferrum-backup-5678-0"));
        assert!(!is_builtin_exclude("normal.txt"));

        // SP-4.4: Volume root special folders
        assert!(is_builtin_exclude("$RECYCLE.BIN"));
        assert!(is_builtin_exclude(r"$recycle.bin\sub"));
        assert!(is_builtin_exclude("System Volume Information"));
        assert!(is_builtin_exclude(r"system volume information\catalog"));
        assert!(!is_builtin_exclude("My Documents"));
    }

    #[test]
    fn test_glob_wildcards() {
        // SP-4.1: * wildcard
        let p_star = Pattern::parse("*.tmp");
        assert!(p_star.matches("test.tmp", false));
        assert!(p_star.matches(r"sub\test.tmp", false));
        assert!(!p_star.matches("test.txt", false));

        // SP-4.1: ? wildcard
        let p_question = Pattern::parse("file?.txt");
        assert!(p_question.matches("file1.txt", false));
        assert!(p_question.matches("fileA.txt", false));
        assert!(!p_question.matches("file12.txt", false));

        // SP-4.1: ** wildcard
        let p_globstar = Pattern::parse(r"docs\**\*.md");
        assert!(p_globstar.matches(r"docs\readme.md", false));
        assert!(p_globstar.matches(r"docs\sub\section\page.md", false));
        assert!(!p_globstar.matches(r"src\readme.md", false));
    }

    #[test]
    fn test_trailing_slash_folders_only() {
        // SP-4.2 & P1-B: Trailing slash matches folders only and does NOT anchor
        let p_dir = Pattern::parse(r"node_modules\");
        assert!(p_dir.matches("node_modules", true));
        assert!(p_dir.matches(r"project\node_modules", true)); // P1-B: unanchored matches at any depth
        assert!(!p_dir.matches("node_modules", false)); // not a dir
        assert!(!p_dir.matches(r"project\node_modules", false));

        // P3-1: Trailing slash with ** matches folder at any depth
        let p_glob_dir = Pattern::parse(r"**\node_modules\");
        assert!(p_glob_dir.matches("node_modules", true));
        assert!(p_glob_dir.matches(r"project\node_modules", true));
        assert!(!p_glob_dir.matches(r"project\node_modules", false));

        // P1-B: Pattern with internal slash is anchored
        let p_anchored_dir = Pattern::parse(r"build\out\");
        assert!(p_anchored_dir.matches(r"build\out", true));
        assert!(!p_anchored_dir.matches(r"sub\build\out", true));
    }

    #[test]
    fn test_case_insensitivity_non_ascii() {
        // SP-4.2: Case-insensitive matching with non-ASCII
        let p = Pattern::parse(r"Über\*.TXT");
        assert!(p.matches(r"über\notes.txt", false));
        assert!(p.matches(r"ÜBER\DOC.TXT", false));
        assert!(!p.matches(r"other\notes.txt", false));
    }

    #[test]
    fn test_case_folding_turkish_i_and_kelvin() {
        // P3-2: Turkish capital I with dot above ('İ', \u{0130}) produces 2 code points in lowercase.
        // It must NOT be mapped to 2 code points; it is kept as 'İ'.
        assert_eq!(fold_char('İ'), 'İ');

        // P3-2: Kelvin sign ('\u{212A}', K) lowercases to exactly one character ('k').
        assert_eq!(fold_char('\u{212A}'), 'k');

        let pat_kelvin = Pattern::parse("\u{212A}.txt");
        assert!(pat_kelvin.matches("k.txt", false));
        assert!(pat_kelvin.matches("K.txt", false));

        let pat_turkish = Pattern::parse("İ.txt");
        assert!(pat_turkish.matches("İ.txt", false));
        assert!(!pat_turkish.matches("i.txt", false));
    }

    #[test]
    fn test_benchmark_fifty_globstars_depth_200() {
        // P2-3: 50x `**\` against path 200 levels deep must execute in < 50 ms in debug build.
        let mut raw_pat = String::new();
        for _ in 0..50 {
            raw_pat.push_str(r"**\");
        }
        raw_pat.push_str("target.txt");
        let pat = Pattern::parse(&raw_pat);

        // Path of depth 200
        let mut path_components = Vec::with_capacity(200);
        for i in 0..199 {
            path_components.push(format!("folder_{i}"));
        }
        path_components.push("target.txt".to_string());
        let full_path = path_components.join(r"\");

        let start = Instant::now();
        let matched = pat.matches(&full_path, false);
        let elapsed = start.elapsed();

        assert!(matched, "pattern must match target at depth 200");
        assert!(
            elapsed.as_millis() < 50,
            "50x globstars took too long: {} ms (must be < 50 ms)",
            elapsed.as_millis()
        );
    }

    #[test]
    fn test_filter_limits_sp_4_2() {
        // SP-4.2: <= 64 patterns
        let max_patterns: Vec<String> = (0..64).map(|i| format!("*.tmp{i}")).collect();
        assert!(ProfileFilter::new(&max_patterns, &[]).is_ok());
        assert!(ProfileFilter::new(&[], &max_patterns).is_ok());

        let too_many: Vec<String> = (0..65).map(|i| format!("*.tmp{i}")).collect();
        assert!(matches!(
            ProfileFilter::new(&too_many, &[]),
            Err(FilterError::TooManyIncludePatterns(65))
        ));
        assert!(matches!(
            ProfileFilter::new(&[], &too_many),
            Err(FilterError::TooManyExcludePatterns(65))
        ));

        // SP-4.2: <= 260 characters per pattern
        let ok_pattern = "a".repeat(260);
        assert!(ProfileFilter::new(&[ok_pattern], &[]).is_ok());

        let too_long_pattern = "a".repeat(261);
        assert!(matches!(
            ProfileFilter::new(&[too_long_pattern], &[]),
            Err(FilterError::PatternTooLong { len: 261, .. })
        ));
    }

    #[test]
    fn test_include_folder_recursively_includes_contents_p2_6() {
        // P2-6: include matching a folder also covers all its contents recursively
        let filter = ProfileFilter::new(&[r"docs\".to_string()], &[]).unwrap();

        assert!(filter.visible("docs", true));
        assert!(filter.visible(r"docs\readme.txt", false));
        assert!(filter.visible(r"docs\specs\arch.pdf", false));
        // An unanchored `docs\` can match at any depth, so other folders stay visible
        // (they are walked to look for nested `docs`), but their files do not.
        assert!(filter.visible("photos", true));
        assert!(!filter.visible(r"photos\pic.jpg", false));
    }

    #[test]
    fn test_include_unanchored_folder_matches_nested() {
        // Include `docs\` without anchor matches at any depth, like exclude.
        let filter = ProfileFilter::new(&[r"docs\".to_string()], &[]).unwrap();

        assert!(filter.visible("a", true)); // must be descended to reach a\docs
        assert!(filter.visible(r"a\docs", true));
        assert!(filter.visible(r"a\docs\readme.txt", false));
        assert!(filter.visible(r"a\b\docs\specs\arch.pdf", false));
        assert!(!filter.visible(r"a\b\photos\pic.jpg", false));
        assert!(!filter.visible(r"a\docs", false)); // a file named docs is not a folder

        // An anchored folder include stays root-only.
        let anchored = ProfileFilter::new(&[r"x\docs\".to_string()], &[]).unwrap();
        assert!(anchored.visible(r"x\docs\f.txt", false));
        assert!(!anchored.visible(r"y\x\docs\f.txt", false));
    }

    #[test]
    fn test_unanchored_matches_basename_any_depth_p3_1() {
        // P3-1: pattern without slash matches basename at any depth (gitignore style)
        let pat_unanchored = Pattern::parse("*.log");
        assert!(pat_unanchored.matches("server.log", false));
        assert!(pat_unanchored.matches(r"app\logs\server.log", false));

        // Pattern with slash is anchored to root
        let pat_anchored = Pattern::parse(r"logs\*.log");
        assert!(pat_anchored.matches(r"logs\server.log", false));
        assert!(!pat_anchored.matches(r"sub\logs\server.log", false));
    }

    #[test]
    fn test_slash_separator_p3_1() {
        // P3-1: / accepted as separator
        let pat = Pattern::parse("docs/spec/*.md");
        assert!(pat.matches(r"docs\spec\design.md", false));
        assert!(pat.matches("docs/spec/design.md", false));
    }

    #[test]
    fn test_include_exclude_interplay() {
        // SP-4.3: Interplay of include and exclude
        let filter = ProfileFilter::new(
            &["*.txt".to_string(), r"docs\**".to_string()],
            &["secret.txt".to_string(), r"docs\private\**".to_string()],
        )
        .unwrap();

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
