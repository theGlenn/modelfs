# Decisions

Dated log of load-bearing choices. Newest last.

## 2026-08-13 — Language: Rust

Covers the whole roadmap: syscall-level filesystem work (clonefile, renamex_np), mmap
GGUF parsing, later FUSE (`fuser`) and daemon. Single static binary. Edition 2024.

## 2026-08-13 — Platform: macOS/APFS first, Linux second

Local-AI usage concentrates on Apple Silicon (MLX is macOS-only). Storage backend goes
behind a trait so Linux reflink (`FICLONE` on btrfs/XFS) and hardlink fallback slot in
later.

## 2026-08-13 — Dedupe mechanism: APFS clonefile, not hardlink

`clonefile(2)` shares extents (same disk savings) but keeps independent inodes:
a provider mutating its copy triggers copy-on-write instead of corrupting every view.
Hardlink remains an opt-in flag later.

Measured on this machine (44 MB Ollama blob, `/System/Volumes/Data`):
clone cost **0 blocks**, full copy cost 89 800 × 512 B blocks. Atomic
clone-to-temp + `rename` preserves content hash.

Consequence: savings accounting can't trust `du` (clones report full logical size).
Measure via allocated-blocks delta / container free space.

## 2026-08-13 — Digest: SHA-256 first-class, algorithm-independent abstraction

SHA-256 is the ecosystem interop digest: Ollama blob filenames ARE sha256; HF LFS blob
names ARE sha256. Harvest identity from names, verify lazily. The `Digest` type carries
an explicit algorithm tag (`sha256:<hex>` rendering) so blake3 or chunk-level schemes
can join without schema breakage. HF 40-hex names are git-SHA-1 — parseable, never
trusted as content identity.

## 2026-08-13 — Xet is a distinct provider architecture

`~/.cache/huggingface/xet` is a chunk-level transfer cache (shards + chunks), not model
storage. Excluded from scan, never modified. See `providers/xet.md`.

## 2026-08-13 — Conservative dedupe protocol

No mtime-only heuristics. Per replacement:

1. Fully hash canonical A and target B; require byte-identical digests.
2. Snapshot B's `stat` (inode, size, mtime); check no open write fds (`lsof`).
3. `clonefile` A → `B.modeld-tmp-<pid>` in B's directory (same volume guaranteed).
4. Re-`stat` B; any change (inode/size/mtime) aborts.
5. Fully hash the temporary clone, then atomically swap with
   `renamex_np(tmp, B, RENAME_SWAP)` — B's original bytes survive under the temp name.
6. Re-check the original inode under the temp name for racing writers/metadata changes;
   roll back on any change.
7. Durably journal the swap before releasing the rollback file, then restore B's
   original mtime where providers key caches on it (LM Studio).
8. `modeld restore` replays the journal with the same writer and stat checks.

Files with `.incomplete`/`.part`/`-partial` markers or active writers are never touched.

## 2026-08-26 — Semantics: header facts are display-only, read once per artifact

`modeld-formats` parses what a weights file says about itself: GGUF `general.*`
metadata (name, architecture, `file_type` → llama.cpp quant name, `size_label`)
and the safetensors JSON prologue (dominant dtype by bytes, parameter count from
shapes). Parsers read headers only — cost independent of model size — and are
bounded: every length field is checked against a sanity cap before allocation,
so corrupt files yield errors, never panics or unbounded reads. GGUF v1 and
big-endian files are rejected as unsupported.

Facts land in registry columns (`kind`, `name`, `architecture`, `quant`,
`params`) at sync time, read from the canonical blob after verification. `kind`
(`model` | `asset`) doubles as the analyzed marker; tokenizers/vocabularies are
classified by reference filename and listed separately in `ls`. Identity stays
with digests — semantics are presentation, never trusted for dedupe or storage
decisions.

Safetensors detection for extensionless blobs (HF cache): a plausible u64 LE
header length followed by `{` is decisive; ASCII JSON sidecars fail the length
check by construction.

## 2026-08-26 — gc: unreferenced blobs are deletable, with two guards

`modeld gc` deletes store blobs that no provider path references. References are
only pruned by a *complete* sync, so zero references is a settled fact, not a
transient one. Two guards keep a blob regardless:

1. Its digest appears in a journaled swap — `restore` rebuilds victims from the
   canonical blob, so gc would break restore.
2. A `shared_paths` row for it still stat-matches on disk — the blob is the
   recorded anchor of a live clone even though no scan currently sees it (e.g.
   a root was removed from the config).

Deleting a blob never touches clone bytes (copy-on-write): existing copies keep
their extents. The blob file is removed before its registry rows, so a failed
delete leaves the artifact intact and retryable. Reported sizes are logical;
physical reclaim depends on whether other files still share the extents.
