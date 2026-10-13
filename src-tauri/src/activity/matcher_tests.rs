#[path = "matcher.rs"]
mod matcher;

use matcher::{
    bounded_damerau_levenshtein, compare_matches, is_initialism, is_subsequence, match_candidate,
    MatchClass,
};
use std::cmp::Ordering;

fn class(query: &str, name: &str, path: &str) -> Option<MatchClass> {
    match_candidate(query, name, path, 0).map(|m| m.class)
}

#[test]
fn unicode_adjacent_transposition_and_edit_bounds() {
    assert_eq!(bounded_damerau_levenshtein("café", "caéf", 1), Some(1));
    assert_eq!(
        bounded_damerau_levenshtein("你好世界", "你世好界", 1),
        Some(1)
    );
    assert_eq!(
        bounded_damerau_levenshtein("activity", "actvity", 1),
        Some(1)
    );
    assert_eq!(bounded_damerau_levenshtein("activity", "actvtiy", 1), None);
    assert_eq!(
        bounded_damerau_levenshtein("activity", "actvtiy", 2),
        Some(2)
    );
    assert_eq!(bounded_damerau_levenshtein("", "", 0), Some(0));
    assert_eq!(bounded_damerau_levenshtein("abc", "abcdef", 2), None);
    assert_eq!(bounded_damerau_levenshtein("CA", "ABC", 2), Some(2));
    assert_eq!(bounded_damerau_levenshtein("CA", "ABC", 1), None);
    assert_eq!(bounded_damerau_levenshtein(&"a".repeat(257), "a", 2), None);
}

fn reference_distance(left: &str, right: &str) -> usize {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let mut distances = vec![vec![0; right.len() + 2]; left.len() + 2];
    let maximum = left.len() + right.len();
    distances[0][0] = maximum;
    for i in 0..=left.len() {
        distances[i + 1][0] = maximum;
        distances[i + 1][1] = i;
    }
    for j in 0..=right.len() {
        distances[0][j + 1] = maximum;
        distances[1][j + 1] = j;
    }
    let mut last_seen = std::collections::HashMap::new();
    for i in 1..=left.len() {
        let mut last_match = 0;
        for j in 1..=right.len() {
            let earlier = *last_seen.get(&right[j - 1]).unwrap_or(&0);
            let cost = usize::from(left[i - 1] != right[j - 1]);
            distances[i + 1][j + 1] = (distances[i][j] + cost)
                .min(distances[i + 1][j] + 1)
                .min(distances[i][j + 1] + 1)
                .min(distances[earlier][last_match] + i - earlier + j - last_match - 1);
            if cost == 0 {
                last_match = j;
            }
        }
        last_seen.insert(left[i - 1], i);
    }
    distances[left.len() + 1][right.len() + 1]
}

#[test]
fn bounded_distance_agrees_with_unbounded_reference() {
    let mut samples = vec![String::new()];
    for _ in 0..4 {
        let extended: Vec<String> = samples
            .iter()
            .flat_map(|prefix| ['a', 'b', 'é'].map(|c| format!("{prefix}{c}")))
            .collect();
        samples.extend(extended);
    }
    for left in &samples {
        for right in &samples {
            let distance = reference_distance(left, right);
            for limit in 0..=3 {
                assert_eq!(
                    bounded_damerau_levenshtein(left, right, limit),
                    (distance <= limit).then_some(distance),
                    "left={left:?}, right={right:?}, limit={limit}"
                );
            }
        }
    }
}

#[test]
fn numeric_suffix_abbreviation_is_not_loose_subsequence() {
    assert!(is_initialism("inv2026", "Invoice 2026"));
    assert_eq!(
        class("inv2026", "Invoice 2026", ""),
        Some(MatchClass::Initialism)
    );
    assert!(!is_initialism("inv2026", "Invoices 2025"));
    assert!(!is_initialism("inv26", "Invoice 2026"));
}

#[test]
fn typo_thresholds_follow_query_token_length() {
    assert_eq!(class("app", "ape", ""), None);
    assert_eq!(class("abcd", "abdc", ""), Some(MatchClass::Fuzzy));
    assert_eq!(class("abcdefg", "abcdeyy", ""), None);
    assert_eq!(class("abcdefgh", "abcdefyy", ""), Some(MatchClass::Fuzzy));
    assert_eq!(class("activity", "actvity", ""), Some(MatchClass::Fuzzy));
    assert_eq!(class("café", "caéf", ""), Some(MatchClass::Fuzzy));
    assert_eq!(class("你好世界", "你世好界", ""), Some(MatchClass::Fuzzy));
    assert_eq!(class("ACTIVITY", "activity", ""), Some(MatchClass::Exact));
}

#[test]
fn matches_distinct_classes_without_fuzzying_short_queries() {
    assert_eq!(
        class("terminal", "terminal", "/bin/terminal"),
        Some(MatchClass::Exact)
    );
    assert_eq!(class("term", "terminal", ""), Some(MatchClass::Prefix));
    assert_eq!(
        class("studio", "Visual Studio Code", ""),
        Some(MatchClass::Token)
    );
    assert_eq!(
        class("local", "misc", "/home/local/misc"),
        Some(MatchClass::Path)
    );
    assert_eq!(
        class("vsc", "Visual Studio Code", ""),
        Some(MatchClass::Initialism)
    );
    assert_eq!(
        class("vsco", "Visual Studio Code", ""),
        Some(MatchClass::Subsequence)
    );
    assert!(is_initialism("vsc", "Visual Studio Code"));
    assert!(!is_initialism("vsco", "Visual Studio Code"));
    assert!(is_subsequence("vsco", "Visual Studio Code"));
    assert!(!is_subsequence("vcs", "Visual Studio Code"));
}

#[test]
fn explicit_paths_only_match_literal_paths() {
    assert_eq!(
        class("/home/me/app", "app", "/home/me/app"),
        Some(MatchClass::ExactPath)
    );
    assert_eq!(
        class("/home/me/ap", "app", "/home/me/app"),
        Some(MatchClass::Path)
    );
    assert_eq!(
        class("\\home\\me\\app", "app", "\\home\\me\\app"),
        Some(MatchClass::ExactPath)
    );
    assert_eq!(class("/home/me/apa", "app", "/home/me/app"), None);
    assert_eq!(class("/home/me/app", "app", "/home/me/App"), None);
    assert_eq!(
        class("/home/me/App", "app", "/home/me/App"),
        Some(MatchClass::ExactPath)
    );
    assert_eq!(class("/home/me/app", "/home/me/app", "/other"), None);
    assert_eq!(class("  ", "app", "/home/me/app"), None);
}

#[test]
fn class_priority_beats_popularity_and_boost_is_bounded() {
    let exact = match_candidate("activity", "activity", "", 0).unwrap();
    let fuzzy = match_candidate("activity", "actvity", "", u32::MAX).unwrap();
    assert_eq!(
        compare_matches(&exact, "activity", &fuzzy, "actvity"),
        Ordering::Less
    );
    let low = match_candidate("activity", "activity", "", 0).unwrap();
    let high = match_candidate("activity", "activity", "", u32::MAX).unwrap();
    assert!(high.score > low.score);
    assert!(high.score - low.score <= 24);
    assert!(high.score <= 1000);
}

#[test]
fn tie_breaks_by_path_for_stable_order() {
    let a = match_candidate("editor", "editor", "/a/editor", 10).unwrap();
    let b = match_candidate("editor", "editor", "/b/editor", 10).unwrap();
    assert_eq!(
        compare_matches(&a, "/a/editor", &b, "/b/editor"),
        Ordering::Less
    );
    assert_eq!(
        compare_matches(&b, "/b/editor", &a, "/a/editor"),
        Ordering::Greater
    );
}
