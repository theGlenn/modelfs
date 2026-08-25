//! Safetensors header parser: dominant tensor precision and parameter count.
//!
//! A safetensors file starts with a `u64` little-endian JSON header length,
//! then that many bytes of JSON mapping tensor names to `{dtype, shape,
//! data_offsets}`. Only the header is read; the parameter count is the sum of
//! shape products, and the reported precision is the dtype covering the most
//! bytes (embeddings are often a different dtype than the bulk of the model).

use crate::{FormatError, ModelInfo, params_label};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

/// Largest JSON header we will read; real headers are well under 10 MiB.
const MAX_HEADER_LEN: u64 = 100 * 1024 * 1024;

/// Reads precision and parameter count from a safetensors file header.
///
/// # Errors
/// [`FormatError::Io`] on read failure, [`FormatError::Malformed`] when the
/// length prefix is implausible or the header is not the expected JSON shape.
pub fn inspect_safetensors(path: &Path) -> Result<ModelInfo, FormatError> {
    let mut file = std::fs::File::open(path)?;
    let mut length_bytes = [0u8; 8];
    file.read_exact(&mut length_bytes)?;
    let header_len = u64::from_le_bytes(length_bytes);
    if header_len == 0 || header_len > MAX_HEADER_LEN {
        return Err(FormatError::Malformed(format!(
            "implausible header length {header_len}"
        )));
    }
    let mut header = vec![0u8; usize::try_from(header_len).unwrap_or(0)];
    file.read_exact(&mut header)?;
    parse_header(&header)
}

fn parse_header(header: &[u8]) -> Result<ModelInfo, FormatError> {
    let tensors: serde_json::Map<String, serde_json::Value> = serde_json::from_slice(header)
        .map_err(|error| FormatError::Malformed(format!("header is not JSON: {error}")))?;

    let mut param_count: u64 = 0;
    // BTreeMap keeps the dominant-dtype tie-break deterministic.
    let mut bytes_by_dtype: BTreeMap<String, u128> = BTreeMap::new();
    for (name, tensor) in &tensors {
        if name == "__metadata__" {
            continue;
        }
        let Some(dtype) = tensor.get("dtype").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let elements = tensor
            .get("shape")
            .and_then(serde_json::Value::as_array)
            .map_or(0u128, |shape| {
                shape
                    .iter()
                    .filter_map(serde_json::Value::as_u64)
                    .map(u128::from)
                    .product()
            });
        param_count = param_count.saturating_add(u64::try_from(elements).unwrap_or(u64::MAX));
        *bytes_by_dtype.entry(dtype.to_string()).or_default() +=
            elements * u128::from(dtype_size(dtype));
    }

    let dominant = bytes_by_dtype
        .into_iter()
        .max_by_key(|(_, bytes)| *bytes)
        .map(|(dtype, _)| dtype);
    Ok(ModelInfo {
        name: None,
        architecture: None,
        quant: dominant,
        params: (param_count > 0).then(|| params_label(param_count)),
    })
}

/// Bytes per element for safetensors dtypes; unknown dtypes count as 1.
fn dtype_size(dtype: &str) -> u64 {
    match dtype {
        "F64" | "I64" | "U64" => 8,
        "F32" | "I32" | "U32" => 4,
        "F16" | "BF16" | "I16" | "U16" => 2,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_safetensors(json: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("model.safetensors");
        let mut file = std::fs::File::create(&path).expect("create file");
        file.write_all(&(json.len() as u64).to_le_bytes())
            .expect("write length");
        file.write_all(json.as_bytes()).expect("write header");
        (dir, path)
    }

    #[test]
    fn reports_dominant_dtype_and_parameter_count() {
        let json = r#"{
            "__metadata__": {"format": "pt"},
            "embed.weight": {"dtype": "F32", "shape": [100, 8], "data_offsets": [0, 3200]},
            "layer.0.weight": {"dtype": "BF16", "shape": [1000, 1000], "data_offsets": [3200, 2003200]}
        }"#;
        let (_dir, path) = write_safetensors(json);

        let info = inspect_safetensors(&path).expect("parse");

        assert_eq!(info.quant.as_deref(), Some("BF16"));
        assert_eq!(info.params.as_deref(), Some("1M")); // 1_000_800 params
    }

    #[test]
    fn rejects_implausible_length_prefix() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("bad.safetensors");
        std::fs::write(&path, u64::MAX.to_le_bytes()).expect("write");

        assert!(matches!(
            inspect_safetensors(&path),
            Err(FormatError::Malformed(_))
        ));
    }

    #[test]
    fn rejects_non_json_header() {
        let dir = tempfile::tempdir().expect("create temp dir");
        let path = dir.path().join("bad.safetensors");
        let mut bytes = 4u64.to_le_bytes().to_vec();
        bytes.extend(b"GGUF");
        std::fs::write(&path, bytes).expect("write");

        assert!(matches!(
            inspect_safetensors(&path),
            Err(FormatError::Malformed(_))
        ));
    }

    #[test]
    fn tensorless_header_yields_empty_info() {
        let (_dir, path) = write_safetensors(r#"{"__metadata__": {}}"#);

        let info = inspect_safetensors(&path).expect("parse");

        assert_eq!(info.quant, None);
        assert_eq!(info.params, None);
    }
}
