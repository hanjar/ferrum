//! Service configuration: `C:\ProgramData\Ferrum\indexer.conf` (FER-474).
//!
//! A tiny `key=value` text file instead of JSON so the parser stays a few
//! lines of reviewable code inside a LocalSystem process. The file lives in a
//! directory whose DACL grants only SYSTEM and Administrators (see
//! `fsacl`), so its content is trusted for integrity — but it is still parsed
//! strictly, and anything unexpected falls back to the privacy-safe default
//! ([`Mode::Shared`]) instead of failing open.

use crate::proto::Mode;

/// File name inside the Ferrum ProgramData directory.
pub const CONFIG_FILE: &str = "indexer.conf";
/// Directory name under `C:\ProgramData`.
pub const DATA_DIR: &str = "Ferrum";
/// Refuse to even read a config larger than this.
pub const MAX_CONFIG_BYTES: u64 = 4096;
const FORMAT_VERSION: &str = "1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    pub mode: Mode,
}

impl Default for Config {
    /// Privacy-safe default: used when no config exists or it is unreadable.
    fn default() -> Self {
        Config { mode: Mode::Shared }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    TooLarge,
    NotUtf8,
    BadLine(usize),
    UnknownKey(usize),
    DuplicateKey(usize),
    BadVersion,
    BadMode,
    MissingKey(&'static str),
    /// The file is not a trusted plain file (planted, linked, wrong owner).
    Untrusted,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge => write!(f, "config larger than {MAX_CONFIG_BYTES} bytes"),
            Self::NotUtf8 => write!(f, "config is not UTF-8"),
            Self::BadLine(n) => write!(f, "line {n}: expected key=value"),
            Self::UnknownKey(n) => write!(f, "line {n}: unknown key"),
            Self::DuplicateKey(n) => write!(f, "line {n}: duplicate key"),
            Self::BadVersion => write!(f, "unsupported config version"),
            Self::BadMode => write!(f, "invalid mode"),
            Self::MissingKey(k) => write!(f, "missing key {k}"),
            Self::Untrusted => write!(f, "config file is not trusted"),
        }
    }
}

impl Config {
    /// Strict parse. Blank lines and `#` comments are allowed; every other
    /// line must be a known `key=value` exactly once.
    pub fn parse(bytes: &[u8]) -> Result<Config, ConfigError> {
        if bytes.len() as u64 > MAX_CONFIG_BYTES {
            return Err(ConfigError::TooLarge);
        }
        let text = std::str::from_utf8(bytes).map_err(|_| ConfigError::NotUtf8)?;
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
        let mut version = None;
        let mut mode = None;
        for (i, raw) in text.lines().enumerate() {
            let n = i + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (k, v) = line.split_once('=').ok_or(ConfigError::BadLine(n))?;
            let (k, v) = (k.trim(), v.trim());
            let slot = match k {
                "version" => &mut version,
                "mode" => &mut mode,
                _ => return Err(ConfigError::UnknownKey(n)),
            };
            if slot.is_some() {
                return Err(ConfigError::DuplicateKey(n));
            }
            *slot = Some(v);
        }
        match version {
            Some(FORMAT_VERSION) => {}
            Some(_) => return Err(ConfigError::BadVersion),
            None => return Err(ConfigError::MissingKey("version")),
        }
        let mode = Mode::parse(mode.ok_or(ConfigError::MissingKey("mode"))?)
            .ok_or(ConfigError::BadMode)?;
        Ok(Config { mode })
    }

    /// Parse, falling back to the privacy-safe default on any error. The
    /// error is returned alongside so the caller can log it.
    pub fn parse_or_default(bytes: &[u8]) -> (Config, Option<ConfigError>) {
        match Config::parse(bytes) {
            Ok(c) => (c, None),
            Err(e) => (Config::default(), Some(e)),
        }
    }

    pub fn serialize(&self) -> String {
        format!(
            "# FerrumIndexer configuration. Managed by Ferrum (Settings) and its installer.\r\n\
             # mode: high_performance (single-user PC) | shared (privacy filtering)\r\n\
             version={FORMAT_VERSION}\r\n\
             mode={}\r\n",
            self.mode.as_str()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        for m in Mode::ALL {
            let c = Config { mode: m };
            assert_eq!(Config::parse(c.serialize().as_bytes()).unwrap(), c);
        }
    }

    #[test]
    fn default_is_privacy_safe() {
        assert_eq!(Config::default().mode, Mode::Shared);
        let (c, e) = Config::parse_or_default(b"garbage");
        assert_eq!(c.mode, Mode::Shared);
        assert!(e.is_some());
    }

    #[test]
    fn tolerant_of_whitespace_comments_and_bom() {
        let c = Config::parse("\u{FEFF}# hi\n\n  version = 1 \nmode=high_performance\n".as_bytes())
            .unwrap();
        assert_eq!(c.mode, Mode::HighPerformance);
    }

    #[test]
    fn strict_errors() {
        use ConfigError::*;
        let cases: &[(&[u8], ConfigError)] = &[
            (b"version=1\nmode=shared\nextra=1\n", UnknownKey(3)),
            (b"version=1\nmode=shared\nmode=shared\n", DuplicateKey(3)),
            (b"version=2\nmode=shared\n", BadVersion),
            (b"version=1\nmode=Shared\n", BadMode),
            (b"version=1\nmode\n", BadLine(2)),
            (b"mode=shared\n", MissingKey("version")),
            (b"version=1\n", MissingKey("mode")),
            (&[0xFF, 0xFE, 0x00], NotUtf8),
        ];
        for (input, want) in cases {
            assert_eq!(Config::parse(input).unwrap_err(), *want);
        }
        let big = vec![b'#'; MAX_CONFIG_BYTES as usize + 1];
        assert_eq!(Config::parse(&big).unwrap_err(), TooLarge);
    }
}
