//! FER-75: pure detection of multipart archive volume names.
//!
//! Recognized layouts (matched case-insensitively on the file name):
//! - `stem.part1.rar`, `stem.part01.rar`, `stem.part2.rar` (RAR new style)
//! - `stem.rar` followed by `stem.r00`, `stem.r01`, ... (RAR legacy style;
//!   `stem.rar` is volume 1, `.r00` is volume 2, `.rNN` is volume NN + 2)
//! - `stem.7z.001`, `stem.7z.002`, ... (7z split)
//!
//! Names are classified only; nothing here touches the file system or the
//! archive parsers. Callers decide what to show the user.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MultipartStyle {
    RarPart,
    RarLegacy,
    SevenZSplit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultipartName {
    /// Base name shared by every volume, without the part suffix (keeps its case).
    pub stem: String,
    pub style: MultipartStyle,
    /// 1-based volume number.
    pub index: u32,
    /// Digit count written in the name (`part01` -> 2). Used to rebuild names.
    pub width: usize,
}

impl MultipartName {
    pub fn is_first(&self) -> bool {
        self.index == 1
    }

    /// File name of volume `index` in this set, same style and digit width.
    pub fn name_for(&self, index: u32) -> String {
        match self.style {
            MultipartStyle::RarPart => {
                format!("{}.part{:0w$}.rar", self.stem, index, w = self.width)
            }
            MultipartStyle::RarLegacy => {
                if index == 1 {
                    format!("{}.rar", self.stem)
                } else {
                    format!("{}.r{:0w$}", self.stem, index - 2, w = self.width.max(2))
                }
            }
            MultipartStyle::SevenZSplit => {
                format!("{}.7z.{:0w$}", self.stem, index, w = self.width.max(3))
            }
        }
    }

    /// File name of the first volume of this set.
    pub fn first_name(&self) -> String {
        self.name_for(1)
    }
}

fn all_digits(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())
}

fn parse_number(s: &str) -> Option<u32> {
    if !all_digits(s) {
        return None;
    }
    s.parse::<u32>().ok()
}

/// Splits `name` on its last dot. Returns (stem, extension) and keeps the
/// stem's original case. A name without a dot, or with an empty stem, yields None.
fn split_last_dot(name: &str) -> Option<(&str, &str)> {
    let dot = name.rfind('.')?;
    if dot == 0 {
        return None;
    }
    Some((&name[..dot], &name[dot + 1..]))
}

/// Detects whether `file_name` (a bare name, not a path) is a multipart volume.
/// Returns None for ordinary single archives such as `photos.rar` or `backup.7z`.
pub fn detect(file_name: &str) -> Option<MultipartName> {
    let (before_last, last) = split_last_dot(file_name)?;
    if all_digits(last) {
        return detect_7z_split(before_last, last);
    }
    let last_lower = last.to_ascii_lowercase();
    if last_lower == "rar" {
        return detect_rar_part(before_last);
    }
    detect_rar_legacy(before_last, &last_lower)
}

/// `stem.7z.NNN`
fn detect_7z_split(before_last: &str, last: &str) -> Option<MultipartName> {
    let (stem, mid) = split_last_dot(before_last)?;
    if !mid.eq_ignore_ascii_case("7z") || stem.is_empty() {
        return None;
    }
    let index = parse_number(last).filter(|&n| n > 0)?;
    Some(MultipartName {
        stem: stem.to_string(),
        style: MultipartStyle::SevenZSplit,
        index,
        width: last.len(),
    })
}

/// `stem.partN.rar`
fn detect_rar_part(before_last: &str) -> Option<MultipartName> {
    let (stem, part) = split_last_dot(before_last)?;
    let lower = part.to_ascii_lowercase();
    let digits = lower.strip_prefix("part")?;
    let index = parse_number(digits).filter(|&n| n > 0)?;
    Some(MultipartName {
        stem: stem.to_string(),
        style: MultipartStyle::RarPart,
        index,
        width: digits.len(),
    })
}

/// `stem.rNN`: legacy RAR volume, NN maps to volume NN + 2 (`.rar` is volume 1).
/// Needs two or more digits, so `.r0` and `.r` alone are not volumes.
fn detect_rar_legacy(before_last: &str, last_lower: &str) -> Option<MultipartName> {
    let digits = last_lower.strip_prefix('r')?;
    if digits.len() < 2 || before_last.is_empty() {
        return None;
    }
    let index = parse_number(digits)?.checked_add(2)?;
    Some(MultipartName {
        stem: before_last.to_string(),
        style: MultipartStyle::RarLegacy,
        index,
        width: digits.len(),
    })
}

/// Given the name of any volume and the names present in the same folder,
/// returns the volume names that are missing between volume 1 and the highest
/// volume present. Empty when the set is complete or the name is not multipart.
/// Matching is case-insensitive, since Windows file names are.
pub fn missing_volumes(volume_name: &str, folder_names: &[&str]) -> Vec<String> {
    let Some(info) = detect(volume_name) else {
        return Vec::new();
    };
    let present_lower: Vec<String> = folder_names
        .iter()
        .map(|n| n.to_ascii_lowercase())
        .collect();
    let mut highest = 0u32;
    let mut present_indices = Vec::new();
    for other in folder_names.iter().filter_map(|n| detect(n)) {
        if other.style == info.style && other.stem.eq_ignore_ascii_case(&info.stem) {
            present_indices.push(other.index);
            highest = highest.max(other.index);
        }
    }
    (1..=highest)
        .filter(|idx| !present_indices.contains(idx))
        .map(|idx| info.name_for(idx))
        .filter(|candidate| !present_lower.contains(&candidate.to_ascii_lowercase()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(name: &str) -> Option<MultipartName> {
        detect(name)
    }

    #[test]
    fn rar_part_numbers_one_and_zero_padded() {
        let a = d("nama.part1.rar").unwrap();
        assert_eq!(a.style, MultipartStyle::RarPart);
        assert_eq!(a.index, 1);
        assert_eq!(a.width, 1);
        assert!(a.is_first());
        assert_eq!(a.stem, "nama");

        let b = d("nama.part01.rar").unwrap();
        assert_eq!(b.index, 1);
        assert_eq!(b.width, 2);
        assert!(b.is_first());
        assert_eq!(b.first_name(), "nama.part01.rar");

        let c = d("nama.part02.rar").unwrap();
        assert_eq!(c.index, 2);
        assert!(!c.is_first());
        assert_eq!(c.first_name(), "nama.part01.rar");
    }

    #[test]
    fn rar_legacy_volumes_map_to_index_plus_two() {
        let r00 = d("nama.r00").unwrap();
        assert_eq!(r00.style, MultipartStyle::RarLegacy);
        assert_eq!(r00.index, 2);
        assert!(!r00.is_first());
        assert_eq!(r00.first_name(), "nama.rar");

        let r01 = d("nama.r01").unwrap();
        assert_eq!(r01.index, 3);
        assert_eq!(r01.name_for(2), "nama.r00");
    }

    #[test]
    fn seven_z_split_volumes() {
        let a = d("data.7z.001").unwrap();
        assert_eq!(a.style, MultipartStyle::SevenZSplit);
        assert_eq!(a.index, 1);
        assert!(a.is_first());
        assert_eq!(a.width, 3);
        let b = d("data.7z.002").unwrap();
        assert_eq!(b.index, 2);
        assert!(!b.is_first());
        assert_eq!(b.first_name(), "data.7z.001");
    }

    #[test]
    fn case_insensitive_extensions_and_keyword() {
        assert_eq!(d("Nama.PART1.RAR").unwrap().index, 1);
        assert_eq!(d("Nama.Part03.Rar").unwrap().index, 3);
        assert_eq!(d("Nama.R05").unwrap().index, 7);
        assert_eq!(d("Data.7Z.010").unwrap().index, 10);
        // The stem keeps its original case.
        assert_eq!(d("Nama.PART1.RAR").unwrap().stem, "Nama");
    }

    #[test]
    fn dotted_stems_keep_all_but_the_suffix() {
        let a = d("laporan.2026.10.part1.rar").unwrap();
        assert_eq!(a.stem, "laporan.2026.10");
        let b = d("my.backup.v2.7z.001").unwrap();
        assert_eq!(b.stem, "my.backup.v2");
        let c = d("a.b.r00").unwrap();
        assert_eq!(c.stem, "a.b");
    }

    #[test]
    fn ordinary_archives_are_not_multipart() {
        assert_eq!(d("photos.rar"), None);
        assert_eq!(d("backup.7z"), None);
        assert_eq!(d("data.zip"), None);
        assert_eq!(d("nama.r0"), None, "a single digit is not a legacy volume");
        assert_eq!(d("nama.r"), None);
        assert_eq!(d("nama.part1.zip"), None);
        assert_eq!(d("nama.part.rar"), None, "part keyword without number");
        assert_eq!(d("nama.partx.rar"), None);
        assert_eq!(d("data.7z.000"), None, "volume 0 does not exist");
        assert_eq!(d("data.001"), None, "no 7z marker in the name");
        assert_eq!(d(".part1.rar"), None, "empty stem");
        assert_eq!(d("noextension"), None);
    }

    #[test]
    fn missing_volumes_reports_gaps_in_order() {
        let folder = [
            "set.part1.rar",
            "set.part2.rar",
            "set.part4.rar",
            "other.txt",
        ];
        let missing = missing_volumes("set.part1.rar", &folder);
        assert_eq!(missing, vec!["set.part3.rar".to_string()]);
    }

    #[test]
    fn missing_volumes_complete_set_is_empty() {
        let folder = ["set.part01.rar", "set.part02.rar", "set.part03.rar"];
        assert!(missing_volumes("set.part02.rar", &folder).is_empty());
    }

    #[test]
    fn missing_volumes_legacy_and_split_sets() {
        let legacy = ["nama.rar", "nama.r01"];
        assert_eq!(
            missing_volumes("nama.r01", &legacy),
            vec!["nama.r00".to_string()]
        );
        let split = ["data.7z.001", "data.7z.003"];
        assert_eq!(
            missing_volumes("data.7z.001", &split),
            vec!["data.7z.002".to_string()]
        );
    }

    #[test]
    fn missing_volumes_is_case_insensitive_and_ignores_other_sets() {
        let folder = ["SET.PART1.RAR", "set.part3.rar", "other.part2.rar"];
        assert_eq!(
            missing_volumes("set.part1.rar", &folder),
            vec!["set.part2.rar".to_string()]
        );
    }

    #[test]
    fn missing_volumes_non_multipart_returns_empty() {
        assert!(missing_volumes("photos.rar", &["photos.rar"]).is_empty());
    }
}
