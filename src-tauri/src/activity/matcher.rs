use std::cmp::Ordering;
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum MatchClass {
    ExactPath,
    Exact,
    Prefix,
    Token,
    Path,
    Initialism,
    Fuzzy,
    Subsequence,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MatchResult {
    pub class: MatchClass,
    pub score: u16,
}

/// Bounded unrestricted Damerau-Levenshtein over Unicode scalar values.
/// Stored display names cap candidate length at 255 bytes.
pub fn bounded_damerau_levenshtein(left: &str, right: &str, limit: usize) -> Option<usize> {
    let left: Vec<char> = left.chars().collect();
    let right: Vec<char> = right.chars().collect();
    let n = left.len();
    let m = right.len();
    if n > 256 || m > 256 || n.abs_diff(m) > limit {
        return None;
    }
    let beyond = limit.saturating_add(1);
    let mut matrix = vec![vec![beyond; m + 2]; n + 2];
    matrix[1][1] = 0;
    for i in 0..=n {
        matrix[i + 1][1] = i.min(beyond);
    }
    for j in 0..=m {
        matrix[1][j + 1] = j.min(beyond);
    }
    let mut seen = HashMap::new();
    for i in 1..=n {
        let mut last_match = 0;
        let start = i.saturating_sub(limit).max(1);
        let end = m.min(i.saturating_add(limit));
        for j in start..=end {
            let prior_row = *seen.get(&right[j - 1]).unwrap_or(&0);
            let prior_column = last_match;
            let cost = usize::from(left[i - 1] != right[j - 1]);
            if cost == 0 {
                last_match = j;
            }
            let distance = matrix[i][j]
                .saturating_add(cost)
                .min(matrix[i + 1][j].saturating_add(1))
                .min(matrix[i][j + 1].saturating_add(1));
            let distance = if prior_row > 0 && prior_column > 0 {
                distance.min(
                    matrix[prior_row][prior_column]
                        .saturating_add(i - prior_row - 1)
                        .saturating_add(1)
                        .saturating_add(j - prior_column - 1),
                )
            } else {
                distance
            };
            matrix[i + 1][j + 1] = distance.min(beyond);
        }
        seen.insert(left[i - 1], i);
    }
    (matrix[n + 1][m + 1] <= limit).then_some(matrix[n + 1][m + 1])
}

fn words(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|part| !part.is_empty())
}

pub fn is_initialism(query: &str, name: &str) -> bool {
    let tokens: Vec<&str> = words(name).collect();
    if query.is_empty() || tokens.len() < 2 {
        return false;
    }
    let initials: String = tokens
        .iter()
        .filter_map(|word| word.chars().next())
        .collect();
    if initials.to_lowercase() == query.to_lowercase() {
        return true;
    }
    let Some((number, preceding)) = tokens.split_last() else {
        return false;
    };
    if !number.chars().all(|c| c.is_ascii_digit()) || preceding.is_empty() {
        return false;
    }
    let query = query.to_lowercase();
    let preceding_initials: String = preceding
        .iter()
        .filter_map(|word| word.chars().next())
        .collect::<String>()
        .to_lowercase();
    let Some(prefix) = query.strip_suffix(number) else {
        return false;
    };
    let Some(remainder) = prefix.strip_prefix(&preceding_initials) else {
        return false;
    };
    remainder.is_empty()
        || (preceding.len() == 1 && preceding[0].to_lowercase().starts_with(prefix))
}

pub fn is_subsequence(query: &str, name: &str) -> bool {
    if query.is_empty() {
        return false;
    }
    let query = query.to_lowercase();
    let name = name.to_lowercase();
    let mut wanted = query.chars().filter(|c| c.is_alphanumeric());
    let mut next = wanted.next();
    if next.is_none() {
        return false;
    }
    for c in name.chars().filter(|c| c.is_alphanumeric()) {
        if Some(c) == next {
            next = wanted.next();
            if next.is_none() {
                return true;
            }
        }
    }
    false
}

fn explicit_path(query: &str) -> bool {
    query.contains('/') || query.contains('\\') || query.as_bytes().get(1) == Some(&b':')
}

/// Scores a supplied candidate only; caller owns candidate retrieval and result limits.
/// `popularity` is a nonnegative usage count, with at most 24 score points of boost.
pub fn match_candidate(
    query: &str,
    name: &str,
    path: &str,
    popularity: u32,
) -> Option<MatchResult> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    // Explicit locators must preserve case; search normalization cannot establish filesystem identity.
    if explicit_path(query) {
        // Windows locators match case-insensitively (LIKE parity in the
        // store), while an ExactPath hit still requires the exact casing.
        let windows = query.as_bytes().get(1) == Some(&b':') || query.starts_with("\\\\");
        let class = if path == query {
            MatchClass::ExactPath
        } else {
            let contains = if windows {
                path.to_lowercase().contains(&query.to_lowercase())
            } else {
                path.contains(query)
            };
            if contains {
                MatchClass::Path
            } else {
                return None;
            }
        };
        return Some(MatchResult {
            class,
            score: if class == MatchClass::ExactPath {
                1000
            } else {
                600
            },
        });
    }
    let query = query.to_lowercase();
    let name = name.to_lowercase();
    let path = path.to_lowercase();
    let mut distance = 0;
    let class = if name == query {
        MatchClass::Exact
    } else if name.starts_with(&query) {
        MatchClass::Prefix
    } else if words(&name).any(|word| word == query) {
        MatchClass::Token
    } else if !path.is_empty() && path.contains(&query) {
        MatchClass::Path
    } else if is_initialism(&query, &name) {
        MatchClass::Initialism
    } else {
        let max_edits = match query.chars().count() {
            0..=3 => 0,
            4..=7 => 1,
            _ => 2,
        };
        let fuzzy = (max_edits > 0)
            .then(|| {
                words(&name)
                    .filter_map(|word| bounded_damerau_levenshtein(&query, word, max_edits))
                    .min()
            })
            .flatten();
        if let Some(edits) = fuzzy {
            distance = edits;
            MatchClass::Fuzzy
        } else if is_subsequence(&query, &name) {
            MatchClass::Subsequence
        } else {
            return None;
        }
    };
    let base: u16 = match class {
        MatchClass::ExactPath => 1000,
        MatchClass::Exact => 900,
        MatchClass::Prefix => 800,
        MatchClass::Token => 700,
        MatchClass::Path => 600,
        MatchClass::Initialism => 500,
        MatchClass::Fuzzy => 400,
        MatchClass::Subsequence => 300,
    };
    let boost = if popularity == 0 {
        0
    } else {
        (u32::BITS - popularity.leading_zeros()).min(24) as u16
    };
    Some(MatchResult {
        class,
        score: base + boost - (distance as u16) * 10,
    })
}

/// Sort best first; use stable candidate keys (for example, full paths) on ties.
pub fn compare_matches(
    left: &MatchResult,
    left_key: &str,
    right: &MatchResult,
    right_key: &str,
) -> Ordering {
    left.class
        .cmp(&right.class)
        .then_with(|| right.score.cmp(&left.score))
        .then_with(|| left_key.to_lowercase().cmp(&right_key.to_lowercase()))
        .then_with(|| left_key.cmp(right_key))
}
