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

## 2026-09-22 — Daemon: settle-gated reconcile passes under a store lock

`modeld daemon` runs in the foreground and keeps the store converged. It
watches every detected scan root with `FSEvents` (via `notify`), plus the store
directory for `config.toml` edits, and runs *passes*. A pass is one
`sync` followed by store-anchored consolidation. There is no per-file event
handling: `FSEvents` coalesces and replays sticky flags (merely hashing a file
comes back as Create/Modify), so events only say *when* to look, never *what*
changed. The pass re-derives everything from the filesystem, which makes it
idempotent. The extra no-op pass right after a busy one is expected and cheap.

When a pass runs (earliest wins): 10 s after events go quiet, at most 2 min
into a steady stream (such as a long download), when a deferred file settles,
and every 15 min regardless. The periodic pass catches missed events and new
glob-matched roots. Each pass re-detects roots and re-arms the watches.

Settle rule: files modified within `RECENT_WRITE_WINDOW` (5 min) are deferred.
They are not imported, and their old references are kept (the pass counts as
incomplete, so nothing is pruned). Providers stage downloads under partial
names, but manual folders and LM Studio's unverified staging path do not, and
a half-written file must never become a blob. `modeld sync` keeps settling off
because a manual command means "now".

Store-anchored consolidation: after sync, any synced file that the registry
does not record as sharing its blob's extents becomes a clone of the blob. That
also covers the single-copy case that `dedupe`'s group planning misses: a
model re-downloaded after its original was deleted still duplicates the blob.
The conservative swap protocol above applies unchanged.

Store lock: `flock(2)` on `~/.modeld/lock`. Every daemon pass and every
mutating command (`sync`, `dedupe`, `restore`, `gc`) holds it. The journal is
rewritten whole on each append, so two unlocked writers could drop an entry and
make a swap unrestorable. Read-only commands (`ls`, `where`, `scan`, `doctor`)
never wait.

Blob verification is cached: the blob's digest goes into the same stamp-keyed
digest cache as provider files. A blob is re-hashed only when its stamp
(inode, size, mtime, ctime) changes. Before this, every sync re-hashed every
blob, so a daemon pass was as slow as a full store read. Silent media
corruption that leaves the stamp alone is out of scope here; that is a job for
a future `doctor --verify`.

Shutdown: the first SIGINT/SIGTERM stops the daemon between passes, and a pass
in flight always finishes, so a swap is never cut off before it is journaled.
A second signal exits immediately.

## 2026-09-22 — File stamps ignore device numbers; APFS confirms untracked clones

The first live daemon dry run planned to re-clone 20 files (7.8 GB) that
were already clones. There were two causes.

1. **macOS reassigns APFS `st_dev` at boot.** `FileStamp` included the device
   number, so after every restart the digest cache, the blob verification
   cache, and every recorded clone all missed. Each sync re-hashed everything:
   that was the slow-sync watch-item, and the first daemon pass took 9 minutes.
   Stamps are always compared for the same path, so inode + size + nanosecond
   mtime/ctime is enough. The `device` columns stay in the schema, written as 0
   and never matched. Warm pass time went from 9 min to under 1 s.
2. **Imports older than `shared_paths` had no clone record.** A blob cloned
   *from* a provider file before the table existed left that file looking like
   a duplicate. The planner now asks APFS: `fcntl(F_LOG2PHYS_EXT)` maps 16
   evenly spaced offsets of both files to device offsets, and clones map to the
   same blocks. A file that already shares its blob is recorded (adopted), not
   replaced. The check is sampled, so a clone that diverged only between
   samples still counts as shared; that only costs missed savings, never
   correctness. Live result: 8 of the 10 remaining candidates were adopted, and
   the 2 real duplicates left (a 10.1 MB Qwen vocab/merges pair) are true
   copies.
