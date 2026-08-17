# MLX / mlx-lm storage semantics

## Short version: MLX has no store of its own

- `mlx-lm` (and most `mlx-community` tooling) downloads via `huggingface_hub` →
  artifacts live in the standard HF hub cache (`models--mlx-community--*`).
  Observed on this machine: `models--mlx-community--Qwen3-0.6B-bf16`.
- LM Studio's MLX engine stores MLX models under `~/.lmstudio/models/<publisher>/<repo>/`
  as a directory of safetensors + `config.json` (see `lmstudio.md`).

So for modeld, "MLX" is not a scan root — it's a **format/lineage dimension**:

- Format: safetensors (MLX-converted weights) + `config.json` with `quantization` block.
- Identity: `mlx-community/X-4bit` is a *derived* artifact of upstream `Org/X`
  (lineage edge for Phase 5; `can_regenerate: true` — `mlx_lm.convert` reproduces it).

## Dedupe relevance

- MLX artifacts dedupe under the HF provider rules (64-hex sha256 blob names for LFS
  files).
- Cross-provider duplication happens when the same mlx-community repo is pulled by both
  mlx-lm (HF cache) and LM Studio (its own tree) — byte-identical safetensors, prime
  dedupe target.
- MLX weights are NOT byte-identical to their GGUF or BF16 siblings — same model,
  different artifacts. Never candidates for exact dedupe; related only in the Phase 5
  model graph.
