//! Model-file introspection: read what a weights file says about itself.
//!
//! Parsers here read only file *headers* (GGUF metadata key-values, the
//! safetensors JSON prologue) — never tensor data — so inspection cost is
//! independent of model size. All parsers are bounded: length fields from the
//! file are checked against sanity caps before any allocation, and a corrupt or
//! truncated file yields a [`FormatError`], never a panic or an unbounded read.
//!
//! The output of every parser is a [`ModelInfo`]: display-oriented strings
//! (model name, architecture, quantization, parameter-count label) suitable for
//! a registry column or a terminal table. Identity stays with digests; nothing
//! in here is trusted for anything but presentation.

mod gguf;
mod safetensors;

#[doc(inline)]
pub use gguf::inspect_gguf;
#[doc(inline)]
pub use safetensors::inspect_safetensors;

/// What a model file reports about itself; every field display-only.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ModelInfo {
    /// Self-declared model name, e.g. `Qwen3.5 0.8B` (GGUF `general.name`).
    pub name: Option<String>,
    /// Architecture family, e.g. `llama` (GGUF `general.architecture`).
    pub architecture: Option<String>,
    /// Quantization or dominant tensor precision, e.g. `Q4_K_M`, `BF16`.
    pub quant: Option<String>,
    /// Parameter-count label, e.g. `1.7B` (declared or computed from shapes).
    pub params: Option<String>,
}

/// Why a file could not be inspected.
#[derive(Debug, thiserror::Error)]
pub enum FormatError {
    #[error("I/O error reading header: {0}")]
    Io(#[from] std::io::Error),
    #[error("malformed header: {0}")]
    Malformed(String),
    #[error("unsupported file: {0}")]
    Unsupported(String),
}

/// Formats a raw parameter count as a short label, e.g. `1.7B`, `494M`.
#[must_use]
#[expect(
    clippy::cast_precision_loss,
    reason = "display-only rounding to one decimal; exact count never shown via f64"
)]
pub fn params_label(count: u64) -> String {
    // Thresholds follow community naming: models are sized in K/M/B/T params.
    const SCALES: [(u64, &str); 4] = [
        (1_000_000_000_000, "T"),
        (1_000_000_000, "B"),
        (1_000_000, "M"),
        (1_000, "K"),
    ];
    for (scale, suffix) in SCALES {
        if count >= scale {
            let value = format!("{:.1}", count as f64 / scale as f64);
            let value = value.strip_suffix(".0").unwrap_or(&value);
            return format!("{value}{suffix}");
        }
    }
    count.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn params_label_uses_community_scales() {
        assert_eq!(params_label(1_720_000_000), "1.7B");
        assert_eq!(params_label(494_034_560), "494M");
        assert_eq!(params_label(270_000_000), "270M");
        assert_eq!(params_label(999), "999");
    }
}
