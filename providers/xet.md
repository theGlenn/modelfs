# Xet cache semantics (Hugging Face chunk store)

Observed on macOS, `~/.cache/huggingface/xet`, 2026-08-13. 94 MB of overhead present
alongside a 4.8 GB hub cache.

## What it is

Xet is HF's chunk-level content-addressed transfer system replacing git-LFS. The local
xet directory is a **download-side cache**, not final model storage: completed files
still materialize into `hub/models--*/blobs/`. Treat xet as its own provider/cache
architecture, never as ordinary model files.

## Layout (observed)

```
~/.cache/huggingface/xet/
├── logs/xet_<timestamp>_<pid>.log
└── <endpoint-hash>/                   # e.g. https___cas_serv-tGqkUaZf_CBPHQ6h
    ├── shard-cache/<64hex>.mdb        # shard metadata (file→chunk mappings)
    ├── chunk-cache/                   # chunk data (appears during transfers; absent here)
    └── staging/shard-session/         # in-progress upload/download state
```

## Rules for modeld

1. **Exclude the entire xet tree from scan.** Nothing here is a model artifact; sizes
   would double-count against hub blobs, and `.mdb` shards are metadata, not weights.
2. **Never dedupe or modify anything under xet/.** Internal consistency is xet's own;
   we'd corrupt transfer state.
3. Report its footprint (it's real disk usage) as provider overhead, not as model bytes.

## Future relevance (Phase 5+)

Xet's chunking (content-defined chunks shared across files) is philosophically the
chunk-level version of what modeld does at file level. Two research directions later:

- Chunk-level dedupe across quantizations (different files, shared chunks) — xet's
  shard format proves this is viable for model weights.
- If HF ever keeps chunk stores resident, "materialize file from local chunks" could
  replace downloads entirely. Watch `hf-xet` crate development.

Both are explicitly out of scope for M0–M4.
