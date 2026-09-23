# LM Studio storage semantics

Observed on macOS, `~/.lmstudio`, 2026-08-13. Machine currently has an empty models
tree (stale index cache references a deleted `gpt-oss-20b` — useful negative finding:
index caches outlive files).

## Layout

```
~/.lmstudio/                                   # install location configurable;
├── models/<publisher>/<repo>/<file>.gguf      #   recorded in .internal/app-install-location.json
│                                              # MLX models: <publisher>/<repo>/ dir of
│                                              #   safetensors + config.json
├── .internal/
│   ├── model-index-cache.json                 # per-model: abs path, size, GGUF metadata, quant
│   ├── gguf-metadata-cache.json               # keyed by (absPath, mtimeMs) !
│   ├── download-jobs-info.json
│   └── temp-downloads/                        # runtime/backend downloads, *.part files
├── hub/                                       # LM Studio Hub artifacts (plugins, presets)
└── bin/lms                                    # CLI
```

## Key facts

- **Plain files, no hashes anywhere.** Neither index cache stores content hashes —
  identity comes only from path + size + GGUF metadata. modeld must hash LM Studio
  files itself (they're the ones that duplicate HF GGUF downloads byte-for-byte, since
  LM Studio downloads straight from HF repos).
- Path convention `publisher/repo/file.gguf` mirrors the HF repo it came from — a
  strong (but unverified per-file) hint for matching against `hub/models--<org>--<repo>`.
- **`gguf-metadata-cache.json` is keyed by absolute path + `mtimeMs`** (float, ms).
  Replacing a file changes mtime → cache miss → re-parse (harmless but churny).
  Dedupe should restore the original mtime after swap to keep caches warm.
- Deleting a model in the app removes the file and (sometimes) leaves empty dirs +
  stale index entries. Empty `<repo>/` dirs are normal; don't treat as corruption.
- Downloads for models land as partial files before rename (temp-downloads observed for
  runtimes; model-download staging path UNVERIFIED — re-check with a real model pull
  before enabling dedupe on this provider).

## Dedupe tolerance

- Inference backends (llama.cpp / MLX engines) open model files read-only, mmap.
  Clone-swap while the model is not loaded is invisible.
- The app rescans `models/` on focus/refresh; content-identical swap with preserved
  mtime should produce zero observable change.
- Sidecar files (`*.json` configs next to MLX safetensors) are small — dedupe only
  large weight files.

## Open questions

- Exact staging path + rename behavior for model downloads (need one real download to
  confirm).
- Whether `lms` CLI or server hold long-lived open fds on model files while idle
  (check `lsof` before swap regardless — that's the conservative protocol anyway).
