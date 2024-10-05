use std::fmt;
use std::str::FromStr;

use arrayvec::ArrayString;
use prae::Wrapper;

const HASH_HEX_LEN: usize = blake3::OUT_LEN * 2;
prae::define! {
    #[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
    pub FileHash: ArrayString<HASH_HEX_LEN>;
    adjust |hash| hash.make_ascii_lowercase();
    ensure |hash| hash.len() == HASH_HEX_LEN && hash.bytes().all(|ch| ch.is_ascii_hexdigit());
}

impl FileHash {
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_bytes()
    }

    #[cfg(test)]
    pub(crate) fn zero_filled() -> Self {
        Self(ArrayString::<HASH_HEX_LEN>::zero_filled())
    }
}

impl AsRef<str> for FileHash {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl FromStr for FileHash {
    type Err = FromStrError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let arr = ArrayString::<HASH_HEX_LEN>::from(value).or(Err(FromStrError::TooLarge(value.len())))?;
        Self::new(arr).map_err(FromStrError::Other)
    }
}

impl fmt::Display for FileHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug)]
pub enum FromStrError {
    TooLarge(usize),
    Other(prae::ConstructionError<FileHash>),
}

impl fmt::Display for FromStrError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "value is not a 64 character long hexadecimal string")
    }
}

impl std::error::Error for FromStrError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uppercase() -> Result<(), FromStrError> {
        let hash_str = "0011223344556677889900AABBCCDDEEFF0011223344556677889900aabbccdd";
        let hash = FileHash::from_str(hash_str)?;
        assert_eq!(
            AsRef::<str>::as_ref(&hash),
            "0011223344556677889900aabbccddeeff0011223344556677889900aabbccdd"
        );
        Ok(())
    }

    #[test]
    fn length_too_small() {
        let hash_str = "001122";
        let result = FileHash::from_str(hash_str);
        assert!(matches!(result, Err(FromStrError::Other(_))));
    }

    #[test]
    fn length_too_large() {
        let hash_str = "0011223344556677889900AABBCCDDEEFF0011223344556677889900aabbccddeeff";
        let result = FileHash::from_str(hash_str);
        assert!(matches!(result, Err(FromStrError::TooLarge(len)) if len == hash_str.len()));
    }

    #[test]
    fn non_hex() {
        let hash_str = "0011223344556677889900aabbccddeeffgg0011223344556677889900aabbcc";
        let result = FileHash::from_str(hash_str);
        assert!(matches!(result, Err(FromStrError::Other(_))));
    }

    #[test]
    fn non_hex_unicode() {
        let hash_str = "あxyz3344556677889900AABBCCDDEEFF0011223344556677889900aabbccdd";
        let result = FileHash::from_str(hash_str);
        assert!(matches!(result, Err(FromStrError::Other(_))));
    }
}
