# Hugging Face hub cache semantics

Verified on macOS, `~/.cache/huggingface/hub`, 2026-08-13. Subjects: `Qwen/Qwen3.5-0.8B`
(safetensors), `unsloth/gemma-4-E4B-it-GGUF` (partial download), others.

The Xet chunk cache is a **separate architecture** — see `xet.md`. This file covers the
classic hub cache only.

## Layout

```
~/.cache/huggingface/hub/              # override: HF_HOME, HF_HUB_CACHE
├── .locks/                            # per-repo download locks
├── CACHEDIR.TAG
└── models--<org>--<name>/             # also datasets--, spaces--
    ├── blobs/<etag>                   # flat, content-addressed by etag
    ├── snapshots/<commit-sha>/<repo files as symlinks into ../../blobs/>
    ├── refs/<branch>                  # text file containing commit sha
    └── .no_exist/                     # negative cache for missing files
```

## Blob naming — two schemes (verified)

| File kind | Etag / blob name | Example seen |
|---|---|---|
| LFS-tracked (large: weights, GGUF) | **64-hex SHA-256 of content** | `04b1c301...4696` (1.7 GB safetensors) |
| Regular git file (configs, tokenizer) | **40-hex git blob SHA-1** (`sha1("blob <len>\0" + content)`) | `9cb811ded6...` (5 KB config.json) |

So: 64-hex names are harvestable SHA-256 identity (verify lazily); 40-hex names are NOT
sha256 — hash those files ourselves if we care (they're small).

Xet caveat: for xet-backed repos the materialized blob should still be named by the LFS
sha256 from repo metadata, but treat the name as a *claim* — conservative dedupe
verifies by hashing before any replacement anyway.

## Download / mutation behavior (verified)

- In-progress: `blobs/<etag>.incomplete` (seen: 15 MB `.incomplete` from an aborted
  GGUF pull). Never treat `*.incomplete` as an artifact.
- Completed blobs are immutable; snapshot layer is pure symlinks (relative,
  `../../blobs/<etag>`).
- `refs/main` updates to a new commit sha on revision change; old blobs linger until
  cache GC (`hf cache delete`, interactive) — orphan blobs are common.

## Dedupe tolerance

- Best-case provider: the symlink indirection means the library already tolerates
  "file is elsewhere". Replacing a `blobs/<etag>` file with a byte-identical APFS clone
  is invisible to `huggingface_hub` — it checks existence and etag name, not inodes.
- `hf_hub_download` re-uses the blob if present; it does NOT re-hash existing blobs.
- MLX ecosystem (`mlx-lm`, `mlx-community/*`) loads straight from this cache — one
  dedupe covers both. See `mlx.md`.

## Open questions

- Exact etag semantics for xet-backed downloads across `huggingface_hub` versions
  (verify per-file by hashing — which we do regardless).
- Windows cache uses copies instead of symlinks when symlinks unavailable — out of
  scope for now (macOS/Linux first).
