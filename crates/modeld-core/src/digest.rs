//! Content digests. SHA-256 is the first-class interop algorithm (Ollama blob names
//! and HF LFS etags are sha256), but the type is algorithm-tagged so other schemes
//! can be added without changing storage schemas or display formats.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::fmt;
use std::io::Read;
use std::path::Path;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum Algorithm {
    Sha256,
    /// Git blob SHA-1 (`sha1("blob <len>\0" + content)`). Appears as 40-hex names in
    /// the HF hub cache for non-LFS files. Recorded for cross-referencing only —
    /// never trusted as content identity for dedupe.
    GitSha1,
}

impl Algorithm {
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Algorithm::Sha256 => "sha256",
            Algorithm::GitSha1 => "gitsha1",
        }
    }

    /// Byte length of a digest under this algorithm.
    #[must_use]
    pub fn digest_len(&self) -> usize {
        match self {
            Algorithm::Sha256 => 32,
            Algorithm::GitSha1 => 20,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DigestError {
    #[error("unknown digest algorithm `{0}`")]
    UnknownAlgorithm(String),
    #[error("invalid hex in digest: {0}")]
    InvalidHex(#[from] hex::FromHexError),
    #[error("digest length {got} does not match {algorithm} (expected {expected} bytes)")]
    LengthMismatch {
        algorithm: &'static str,
        expected: usize,
        got: usize,
    },
    #[error("digest missing `<algorithm>:` prefix: `{0}`")]
    MissingPrefix(String),
}

/// An algorithm-tagged content digest. Canonical rendering is `<algorithm>:<lowercase hex>`,
/// e.g. `sha256:797b70c4...`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Digest {
    algorithm: Algorithm,
    bytes: Vec<u8>,
}

impl Digest {
    /// Wraps raw digest bytes, validating length for the algorithm.
    ///
    /// # Errors
    /// Returns [`DigestError::LengthMismatch`] if `bytes` has the wrong length.
    pub fn new(algorithm: Algorithm, bytes: Vec<u8>) -> Result<Self, DigestError> {
        if bytes.len() != algorithm.digest_len() {
            return Err(DigestError::LengthMismatch {
                algorithm: algorithm.as_str(),
                expected: algorithm.digest_len(),
                got: bytes.len(),
            });
        }
        Ok(Self { algorithm, bytes })
    }

    /// Parse a bare hex string whose algorithm is implied by context (e.g. an Ollama
    /// `sha256-<hex>` blob filename after stripping the prefix).
    ///
    /// # Errors
    /// Returns [`DigestError`] on invalid hex or wrong length for the algorithm.
    pub fn from_hex(algorithm: Algorithm, hex_str: &str) -> Result<Self, DigestError> {
        Self::new(algorithm, hex::decode(hex_str)?)
    }

    #[must_use]
    pub fn algorithm(&self) -> Algorithm {
        self.algorithm
    }

    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(&self.bytes)
    }

    /// Streaming SHA-256 of a file.
    ///
    /// # Errors
    /// Returns the underlying I/O error if the file cannot be opened or read.
    pub fn sha256_file(path: &Path) -> std::io::Result<Self> {
        let mut file = std::fs::File::open(path)?;
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = file.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
        }
        Ok(Self {
            algorithm: Algorithm::Sha256,
            bytes: hasher.finalize().to_vec(),
        })
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algorithm.as_str(), self.to_hex())
    }
}

impl FromStr for Digest {
    type Err = DigestError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (algo, hex_str) = s
            .split_once(':')
            .ok_or_else(|| DigestError::MissingPrefix(s.to_string()))?;
        let algorithm = match algo {
            "sha256" => Algorithm::Sha256,
            "gitsha1" => Algorithm::GitSha1,
            other => return Err(DigestError::UnknownAlgorithm(other.to_string())),
        };
        Self::from_hex(algorithm, hex_str)
    }
}

impl From<Digest> for String {
    fn from(d: Digest) -> String {
        d.to_string()
    }
}

impl TryFrom<String> for Digest {
    type Error = DigestError;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        s.parse()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn roundtrip_display_parse() {
        let d = Digest::from_hex(
            Algorithm::Sha256,
            "797b70c4edf85907fe0a49eb85811256f65fa0f7bf52166b147fd16be2be4662",
        )
        .unwrap();
        let s = d.to_string();
        assert!(s.starts_with("sha256:797b70c4"));
        assert_eq!(s.parse::<Digest>().unwrap(), d);
    }

    #[test]
    fn rejects_wrong_length() {
        assert!(matches!(
            Digest::from_hex(Algorithm::Sha256, "abcd"),
            Err(DigestError::LengthMismatch { .. })
        ));
    }

    #[test]
    fn rejects_unknown_algorithm() {
        assert!(matches!(
            "blake3:00".parse::<Digest>(),
            Err(DigestError::UnknownAlgorithm(_))
        ));
    }

    #[test]
    fn sha256_file_matches_known_vector() {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"abc").unwrap();
        let d = Digest::sha256_file(f.path()).unwrap();
        assert_eq!(
            d.to_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
