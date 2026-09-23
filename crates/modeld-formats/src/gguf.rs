//! Bounded GGUF header parser: reads `general.*` metadata, skips everything else.
//!
//! GGUF layout (v2/v3, little-endian): magic `GGUF`, `u32` version, `u64`
//! tensor count, `u64` key-value count, then that many `(string key, u32 type,
//! value)` entries. Values we do not care about are skipped by seeking, so a
//! multi-gigabyte model costs a few small reads. Vocabulary string arrays are
//! walked entry-by-entry (each is a `u64` length + seek), which stays in the
//! low milliseconds through a buffered reader.

use crate::{FormatError, ModelInfo};
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// A well-formed model never has more metadata entries than this.
///
/// Real files carry a few hundred; the cap only rejects corrupt counts before
/// they turn into a near-infinite parse loop.
const MAX_KV_COUNT: u64 = 1_000_000;

/// Longest metadata string we will materialize or skip.
///
/// The largest legitimate strings are chat templates (tens of KB); 64 MiB
/// rejects corrupt lengths before a huge allocation or seek past EOF matters.
const MAX_STRING_LEN: u64 = 64 * 1024 * 1024;

/// Longest metadata array we will walk.
///
/// Tokenizer vocabularies are the largest real arrays (a few hundred thousand
/// entries); this cap rejects corrupt counts.
const MAX_ARRAY_LEN: u64 = 10_000_000;

/// Maximum array nesting; real files use depth 1, the spec allows nesting.
const MAX_DEPTH: u32 = 3;

/// Reads model name, architecture, and quantization from a GGUF file header.
///
/// # Errors
/// [`FormatError::Io`] on read failure, [`FormatError::Malformed`] on a corrupt
/// header, [`FormatError::Unsupported`] for GGUF v1 or big-endian files.
pub fn inspect_gguf(path: &Path) -> Result<ModelInfo, FormatError> {
    let file = std::fs::File::open(path)?;
    parse(&mut BufReader::new(file))
}

fn parse<R: Read + Seek>(reader: &mut R) -> Result<ModelInfo, FormatError> {
    let mut magic = [0u8; 4];
    reader.read_exact(&mut magic)?;
    if &magic != b"GGUF" {
        return Err(FormatError::Malformed("not a GGUF file".to_string()));
    }
    let version = read_u32(reader)?;
    // A big-endian file byte-swaps the version into an implausibly huge number.
    if version > 1000 {
        return Err(FormatError::Unsupported(
            "big-endian GGUF not supported".to_string(),
        ));
    }
    if version < 2 {
        return Err(FormatError::Unsupported(format!(
            "GGUF v{version} uses 32-bit counts; only v2+ supported"
        )));
    }
    let _tensor_count = read_u64(reader)?;
    let kv_count = read_u64(reader)?;
    if kv_count > MAX_KV_COUNT {
        return Err(FormatError::Malformed(format!(
            "implausible metadata count {kv_count}"
        )));
    }

    let mut harvest = Harvest::default();
    for _ in 0..kv_count {
        let key = read_string(reader, true)?.unwrap_or_default();
        let value_type = read_u32(reader)?;
        let wanted = wanted_key(&key);
        match read_value(reader, value_type, wanted, 0)? {
            Value::Str(text) => harvest.offer_string(&key, text),
            Value::Uint(number) => harvest.offer_uint(&key, number),
            Value::Skipped => {}
        }
        if harvest.complete() {
            break;
        }
    }
    Ok(harvest.into_info())
}

/// The `general.*` keys worth extracting, collected across the KV walk.
#[derive(Default)]
struct Harvest {
    name: Option<String>,
    architecture: Option<String>,
    size_label: Option<String>,
    file_type: Option<u64>,
}

/// Whether a metadata key is one the harvest extracts.
fn wanted_key(key: &str) -> bool {
    matches!(
        key,
        "general.name" | "general.architecture" | "general.size_label" | "general.file_type"
    )
}

impl Harvest {
    fn offer_string(&mut self, key: &str, text: String) {
        match key {
            "general.name" => self.name = Some(text),
            "general.architecture" => self.architecture = Some(text),
            "general.size_label" => self.size_label = Some(text),
            _ => {}
        }
    }

    fn offer_uint(&mut self, key: &str, number: u64) {
        if key == "general.file_type" {
            self.file_type = Some(number);
        }
    }

    fn complete(&self) -> bool {
        self.name.is_some()
            && self.architecture.is_some()
            && self.size_label.is_some()
            && self.file_type.is_some()
    }

    fn into_info(self) -> ModelInfo {
        ModelInfo {
            name: self.name,
            architecture: self.architecture,
            quant: self.file_type.and_then(file_type_name).map(str::to_string),
            params: self.size_label,
        }
    }
}

enum Value {
    Uint(u64),
    Str(String),
    Skipped,
}

fn read_value<R: Read + Seek>(
    reader: &mut R,
    value_type: u32,
    wanted: bool,
    depth: u32,
) -> Result<Value, FormatError> {
    // GGUF metadata value types, per the GGUF spec.
    match value_type {
        // u8, i8, bool
        0 | 1 | 7 => {
            let mut byte = [0u8; 1];
            reader.read_exact(&mut byte)?;
            Ok(Value::Uint(u64::from(byte[0])))
        }
        // u16, i16
        2 | 3 => {
            let mut bytes = [0u8; 2];
            reader.read_exact(&mut bytes)?;
            Ok(Value::Uint(u64::from(u16::from_le_bytes(bytes))))
        }
        // u32, i32
        4 | 5 => Ok(Value::Uint(u64::from(read_u32(reader)?))),
        // f32
        6 => skip(reader, 4).map(|()| Value::Skipped),
        // string
        8 => Ok(read_string(reader, wanted)?.map_or(Value::Skipped, Value::Str)),
        // array: element type, count, then elements
        9 => {
            if depth >= MAX_DEPTH {
                return Err(FormatError::Malformed("array nested too deep".to_string()));
            }
            let element_type = read_u32(reader)?;
            let count = read_u64(reader)?;
            if count > MAX_ARRAY_LEN {
                return Err(FormatError::Malformed(format!(
                    "implausible array length {count}"
                )));
            }
            match element_scalar_size(element_type) {
                Some(size) => skip(reader, count.saturating_mul(size))?,
                None => {
                    for _ in 0..count {
                        read_value(reader, element_type, false, depth + 1)?;
                    }
                }
            }
            Ok(Value::Skipped)
        }
        // u64, i64
        10 | 11 => Ok(Value::Uint(read_u64(reader)?)),
        // f64
        12 => skip(reader, 8).map(|()| Value::Skipped),
        other => Err(FormatError::Malformed(format!(
            "unknown metadata value type {other}"
        ))),
    }
}

/// Byte width of fixed-size element types, letting arrays skip in one seek.
fn element_scalar_size(element_type: u32) -> Option<u64> {
    match element_type {
        0 | 1 | 7 => Some(1),
        2 | 3 => Some(2),
        4..=6 => Some(4),
        10..=12 => Some(8),
        _ => None,
    }
}

/// Reads a GGUF string, materializing it only when `wanted`.
fn read_string<R: Read + Seek>(
    reader: &mut R,
    wanted: bool,
) -> Result<Option<String>, FormatError> {
    let length = read_u64(reader)?;
    if length > MAX_STRING_LEN {
        return Err(FormatError::Malformed(format!(
            "implausible string length {length}"
        )));
    }
    if !wanted {
        skip(reader, length)?;
        return Ok(None);
    }
    let length = usize::try_from(length)
        .map_err(|_overflow| FormatError::Malformed(format!("string length {length} too large")))?;
    let mut bytes = vec![0u8; length];
    reader.read_exact(&mut bytes)?;
    Ok(Some(String::from_utf8_lossy(&bytes).into_owned()))
}

fn read_u32<R: Read>(reader: &mut R) -> Result<u32, FormatError> {
    let mut bytes = [0u8; 4];
    reader.read_exact(&mut bytes)?;
    Ok(u32::from_le_bytes(bytes))
}

fn read_u64<R: Read>(reader: &mut R) -> Result<u64, FormatError> {
    let mut bytes = [0u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

fn skip<R: Seek>(reader: &mut R, bytes: u64) -> Result<(), FormatError> {
    let offset = i64::try_from(bytes).map_err(|_overflow| {
        FormatError::Malformed(format!("implausible skip of {bytes} bytes"))
    })?;
    reader.seek(SeekFrom::Current(offset))?;
    Ok(())
}

/// Maps GGUF `general.file_type` to its llama.cpp quantization name.
///
/// Numbers are the `LLAMA_FTYPE_*` enum values; gaps are removed variants.
/// Unknown values return `None` rather than inventing a label.
fn file_type_name(file_type: u64) -> Option<&'static str> {
    match file_type {
        0 => Some("F32"),
        1 => Some("F16"),
        2 => Some("Q4_0"),
        3 => Some("Q4_1"),
        7 => Some("Q8_0"),
        8 => Some("Q5_0"),
        9 => Some("Q5_1"),
        10 => Some("Q2_K"),
        11 => Some("Q3_K_S"),
        12 => Some("Q3_K_M"),
        13 => Some("Q3_K_L"),
        14 => Some("Q4_K_S"),
        15 => Some("Q4_K_M"),
        16 => Some("Q5_K_S"),
        17 => Some("Q5_K_M"),
        18 => Some("Q6_K"),
        19 => Some("IQ2_XXS"),
        20 => Some("IQ2_XS"),
        21 => Some("Q2_K_S"),
        22 => Some("IQ3_XS"),
        23 => Some("IQ3_XXS"),
        24 => Some("IQ1_S"),
        25 => Some("IQ4_NL"),
        26 => Some("IQ3_S"),
        27 => Some("IQ3_M"),
        28 => Some("IQ2_S"),
        29 => Some("IQ2_M"),
        30 => Some("IQ4_XS"),
        31 => Some("IQ1_M"),
        32 => Some("BF16"),
        36 => Some("TQ1_0"),
        37 => Some("TQ2_0"),
        38 => Some("MXFP4_MOE"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Builds a minimal valid GGUF v3 header byte-by-byte.
    struct GgufBuilder {
        entries: Vec<Vec<u8>>,
    }

    impl GgufBuilder {
        fn new() -> Self {
            Self {
                entries: Vec::new(),
            }
        }

        fn kv(mut self, key: &str, mut value: Vec<u8>) -> Self {
            let mut entry = gguf_string(key);
            entry.append(&mut value);
            self.entries.push(entry);
            self
        }

        fn string(self, key: &str, text: &str) -> Self {
            let mut value = 8u32.to_le_bytes().to_vec();
            value.extend(gguf_string(text));
            self.kv(key, value)
        }

        fn uint32(self, key: &str, number: u32) -> Self {
            let mut value = 4u32.to_le_bytes().to_vec();
            value.extend(number.to_le_bytes());
            self.kv(key, value)
        }

        fn string_array(self, key: &str, texts: &[&str]) -> Self {
            let mut value = 9u32.to_le_bytes().to_vec();
            value.extend(8u32.to_le_bytes());
            value.extend((texts.len() as u64).to_le_bytes());
            for text in texts {
                value.extend(gguf_string(text));
            }
            self.kv(key, value)
        }

        fn build(self) -> Vec<u8> {
            let mut bytes = b"GGUF".to_vec();
            bytes.extend(3u32.to_le_bytes());
            bytes.extend(0u64.to_le_bytes()); // tensor count
            bytes.extend((self.entries.len() as u64).to_le_bytes());
            for entry in self.entries {
                bytes.extend(entry);
            }
            bytes
        }
    }

    fn gguf_string(text: &str) -> Vec<u8> {
        let mut bytes = (text.len() as u64).to_le_bytes().to_vec();
        bytes.extend(text.as_bytes());
        bytes
    }

    fn write_temp(bytes: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("model.gguf");
        let mut file = std::fs::File::create(&path).expect("create file");
        file.write_all(bytes).expect("write");
        (dir, path)
    }

    #[test]
    fn harvests_name_architecture_and_quant() {
        let bytes = GgufBuilder::new()
            .string("general.architecture", "llama")
            .string("general.name", "Test Model 1.7B")
            .string_array("tokenizer.ggml.tokens", &["<s>", "</s>", "hello"])
            .uint32("general.file_type", 15)
            .string("general.size_label", "1.7B")
            .build();
        let (_dir, path) = write_temp(&bytes);

        let info = inspect_gguf(&path).expect("parse");

        assert_eq!(info.name.as_deref(), Some("Test Model 1.7B"));
        assert_eq!(info.architecture.as_deref(), Some("llama"));
        assert_eq!(info.quant.as_deref(), Some("Q4_K_M"));
        assert_eq!(info.params.as_deref(), Some("1.7B"));
    }

    #[test]
    fn skips_unknown_keys_without_reading_their_values() {
        let bytes = GgufBuilder::new()
            .uint32("some.other.key", 42)
            .string("general.architecture", "qwen3")
            .build();
        let (_dir, path) = write_temp(&bytes);

        let info = inspect_gguf(&path).expect("parse");

        assert_eq!(info.architecture.as_deref(), Some("qwen3"));
        assert_eq!(info.name, None);
    }

    #[test]
    fn truncated_file_is_an_error_not_a_panic() {
        let full = GgufBuilder::new()
            .string("general.name", "Truncated")
            .build();
        let (_dir, path) = write_temp(&full[..full.len() - 5]);

        assert!(matches!(inspect_gguf(&path), Err(FormatError::Io(_))));
    }

    #[test]
    fn rejects_wrong_magic() {
        let (_dir, path) = write_temp(b"NOTG\x03\x00\x00\x00");
        assert!(matches!(
            inspect_gguf(&path),
            Err(FormatError::Malformed(_))
        ));
    }

    #[test]
    fn rejects_implausible_kv_count() {
        let mut bytes = b"GGUF".to_vec();
        bytes.extend(3u32.to_le_bytes());
        bytes.extend(0u64.to_le_bytes());
        bytes.extend(u64::MAX.to_le_bytes());
        let (_dir, path) = write_temp(&bytes);

        assert!(matches!(
            inspect_gguf(&path),
            Err(FormatError::Malformed(_))
        ));
    }

    #[test]
    fn rejects_v1_and_big_endian() {
        for version_bytes in [1u32.to_le_bytes(), 3u32.to_be_bytes()] {
            let mut bytes = b"GGUF".to_vec();
            bytes.extend(version_bytes);
            bytes.extend([0u8; 16]);
            let (_dir, path) = write_temp(&bytes);
            assert!(matches!(
                inspect_gguf(&path),
                Err(FormatError::Unsupported(_))
            ));
        }
    }

    #[test]
    fn unknown_file_type_yields_no_quant() {
        let bytes = GgufBuilder::new().uint32("general.file_type", 999).build();
        let (_dir, path) = write_temp(&bytes);

        assert_eq!(inspect_gguf(&path).expect("parse").quant, None);
    }
}
