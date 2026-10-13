//! Strict argv validation for `ferrum-scan-helper.exe` (FER-460).
//!
//! The ONLY accepted command line is, in this exact order:
//!
//! ```text
//! ferrum-scan-helper.exe --volume <A-Z> --pipe ferrum-scan-<32 hex> --nonce <64 hex>
//! ```
//!
//! Anything else — extra/missing/reordered/duplicated arguments, lowercase
//! hex mixed with uppercase, a path instead of a letter, a pipe name with a
//! `\` — is rejected before the helper touches any handle. The pipe prefix
//! `\\.\pipe\` is added by the helper itself, so argv can never point it at
//! a remote or arbitrary object.

use crate::protocol::NONCE_LEN;

pub const PIPE_PREFIX: &str = "ferrum-scan-";
const PIPE_HEX_LEN: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperArgs {
    /// Uppercase ASCII drive letter.
    pub volume: char,
    /// Bare pipe name (`ferrum-scan-<hex>`), without `\\.\pipe\`.
    pub pipe_name: String,
    pub nonce: [u8; NONCE_LEN],
}

impl HelperArgs {
    /// Full pipe path the helper connects to (always local).
    pub fn pipe_path(&self) -> String {
        format!(r"\\.\pipe\{}", self.pipe_name)
    }

    /// Render the exact argv tail Ferrum passes to the helper.
    pub fn to_command_line(&self) -> String {
        format!(
            "--volume {} --pipe {} --nonce {}",
            self.volume,
            self.pipe_name,
            hex_lower(&self.nonce)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgsError {
    WrongCount(usize),
    UnexpectedFlag(usize),
    BadVolume,
    BadPipe,
    BadNonce,
}

impl std::fmt::Display for ArgsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::WrongCount(n) => write!(f, "expected 6 arguments, got {n}"),
            Self::UnexpectedFlag(i) => write!(f, "unexpected argument at position {i}"),
            Self::BadVolume => write!(f, "volume must be one drive letter A-Z"),
            Self::BadPipe => write!(f, "pipe must be ferrum-scan-<32 lowercase hex>"),
            Self::BadNonce => write!(f, "nonce must be 64 lowercase hex"),
        }
    }
}

pub fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 15) as usize] as char);
    }
    s
}

fn is_lower_hex(s: &str, len: usize) -> bool {
    s.len() == len
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn parse_hex_nonce(s: &str) -> Option<[u8; NONCE_LEN]> {
    if !is_lower_hex(s, NONCE_LEN * 2) {
        return None;
    }
    let digit = |b: u8| {
        if b.is_ascii_digit() {
            b - b'0'
        } else {
            b - b'a' + 10
        }
    };
    let raw = s.as_bytes();
    let mut out = [0u8; NONCE_LEN];
    for (i, o) in out.iter_mut().enumerate() {
        *o = (digit(raw[2 * i]) << 4) | digit(raw[2 * i + 1]);
    }
    Some(out)
}

/// Validate a pipe name produced by Ferrum (`ferrum-scan-<32 hex>`).
pub fn valid_pipe_name(s: &str) -> bool {
    s.strip_prefix(PIPE_PREFIX)
        .is_some_and(|rest| is_lower_hex(rest, PIPE_HEX_LEN))
}

/// Parse argv WITHOUT the program name (i.e. `std::env::args().skip(1)`).
pub fn parse<I, S>(args: I) -> Result<HelperArgs, ArgsError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let v: Vec<String> = args.into_iter().map(|s| s.as_ref().to_string()).collect();
    if v.len() != 6 {
        return Err(ArgsError::WrongCount(v.len()));
    }
    for (i, flag) in [(0, "--volume"), (2, "--pipe"), (4, "--nonce")] {
        if v[i] != flag {
            return Err(ArgsError::UnexpectedFlag(i));
        }
    }
    let vol = v[1].as_bytes();
    if vol.len() != 1 || !vol[0].is_ascii_uppercase() {
        return Err(ArgsError::BadVolume);
    }
    if !valid_pipe_name(&v[3]) {
        return Err(ArgsError::BadPipe);
    }
    let nonce = parse_hex_nonce(&v[5]).ok_or(ArgsError::BadNonce)?;
    Ok(HelperArgs {
        volume: vol[0] as char,
        pipe_name: v[3].clone(),
        nonce,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PIPE: &str = "ferrum-scan-0123456789abcdef0123456789abcdef";
    const NONCE: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    fn ok() -> Vec<&'static str> {
        vec!["--volume", "C", "--pipe", PIPE, "--nonce", NONCE]
    }

    #[test]
    fn accepts_exact_form_and_roundtrips() {
        let a = parse(ok()).unwrap();
        assert_eq!(a.volume, 'C');
        assert_eq!(a.pipe_path(), format!(r"\\.\pipe\{PIPE}"));
        assert_eq!(a.nonce[1], 0x11);
        let line = a.to_command_line();
        let again = parse(line.split(' ')).unwrap();
        assert_eq!(again, a);
    }

    #[test]
    fn rejects_wrong_count_and_order() {
        assert_eq!(parse(Vec::<&str>::new()), Err(ArgsError::WrongCount(0)));
        let mut extra = ok();
        extra.push("--write");
        assert_eq!(parse(extra), Err(ArgsError::WrongCount(7)));
        let swapped = vec!["--pipe", PIPE, "--volume", "C", "--nonce", NONCE];
        assert_eq!(parse(swapped), Err(ArgsError::UnexpectedFlag(0)));
        let dup = vec!["--volume", "C", "--volume", "D", "--nonce", NONCE];
        assert_eq!(parse(dup), Err(ArgsError::UnexpectedFlag(2)));
        let eq_form = vec!["--volume=C", "x", "--pipe", PIPE, "--nonce", NONCE];
        assert_eq!(parse(eq_form), Err(ArgsError::UnexpectedFlag(0)));
    }

    #[test]
    fn rejects_bad_volume() {
        for bad in ["c", "C:", "C:\\", "CD", "", "1", "\\\\server", "Ä"] {
            let mut a = ok();
            a[1] = bad;
            assert_eq!(parse(a), Err(ArgsError::BadVolume), "{bad:?}");
        }
    }

    #[test]
    fn rejects_bad_pipe() {
        for bad in [
            "ferrum-scan-",
            "ferrum-scan-0123456789ABCDEF0123456789ABCDEF",
            "ferrum-scan-0123456789abcdef0123456789abcde",
            "ferrum-scan-0123456789abcdef0123456789abcdef0",
            r"\\.\pipe\ferrum-scan-0123456789abcdef0123456789abcdef",
            r"..\ferrum-scan-0123456789abcdef0123456789abcdef",
            "other-0123456789abcdef0123456789abcdef",
            "ferrum-scan-0123456789abcdef0123456789abcdeg",
        ] {
            let mut a = ok();
            a[3] = bad;
            assert_eq!(parse(a), Err(ArgsError::BadPipe), "{bad:?}");
        }
    }

    #[test]
    fn rejects_bad_nonce() {
        let upper = NONCE.to_uppercase();
        let short = &NONCE[..63];
        let long = format!("{NONCE}0");
        for bad in [upper.as_str(), short, long.as_str(), "", "zz"] {
            let mut a: Vec<String> = ok().iter().map(|s| s.to_string()).collect();
            a[5] = bad.to_string();
            assert_eq!(parse(a), Err(ArgsError::BadNonce), "{bad:?}");
        }
    }

    #[test]
    fn hex_roundtrip() {
        let bytes: Vec<u8> = (0..=255).collect();
        let h = hex_lower(&bytes[..32]);
        assert_eq!(parse_hex_nonce(&h).unwrap().to_vec(), bytes[..32].to_vec());
    }
}
