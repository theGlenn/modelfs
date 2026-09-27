# Design decisions

How ModelFS works, and why it works that way. The [README](README.md) covers
using it; this document covers the reasoning a contributor or a careful user
needs: what ModelFS guarantees, which trade-offs it makes, and what is not
built yet.

When a change alters a choice described here, update this document in the
same PR. The dated log this document replaced is in the git history.

## Principles

Every choice below follows from four rules, in priority order:

1. **Never lose or corrupt a file.** When ModelFS is unsure, it skips. A missed
   saving costs disk space; a wrong replacement costs someone's data.
2. **Apps keep their paths.** ModelFS never moves a file an app owns or turns
   it into a symlink. A duplicate is replaced at the same path by a clone with
   the same bytes, permissions, and modification time.
3. **Everything is undoable.** Every replacement is journaled, and
   `modeld restore` rebuilds independent copies.
4. **Trust only hashes ModelFS computed itself.** Filenames and metadata that
   claim a digest save work; they never decide what gets replaced.

## Why APFS clones

`clonefile(2)` creates a new file that shares the original's disk blocks. Both
files keep their own inode, so when an app rewrites its copy, APFS copies the
changed blocks (copy-on-write) and every other copy stays intact. A hardlink
gives the same saving, but it is one inode under two names: an app that
rewrites its file in place would silently change every other app's copy.

Measured on a 44 MB Ollama blob: a clone cost 0 blocks, and a full copy cost
89 800 blocks of 512 B.

Consequences:

- **Savings are invisible to `du` and Finder**, which count each clone at full
  size. The real saving shows up as free space on the volume (`df`).
- **Clones only work within one volume.** A model file on a different volume
  from `~/.modeld` is skipped.
- **macOS first.** Local-AI work concentrates on Apple Silicon, and MLX is
  macOS-only. Linux can follow with `FICLONE` reflinks on btrfs or XFS. Today
  the APFS calls live directly in `modeld-core/src/apfs.rs`; a storage trait
  comes with the second backend.

## When two files are the same

**Identity is the SHA-256 of the full contents.** SHA-256 is what the
ecosystem already uses: Ollama and Hugging Face LFS name their blobs by it.
The `Digest` type carries its algorithm (`sha256:<hex>`), so a faster hash or
a chunk-level scheme can be added without changing the schema.

**A name only claims a digest.** Two files can claim the same hash, and a
Hugging Face 40-hex name is a git SHA-1 that never matches the SHA-256 of an
identical copy. Harvested names only save work: a file whose size nothing else
shares cannot have a duplicate, so `scan` and `dedupe` do not hash it. Files
with colliding sizes are hashed, and only digests ModelFS computed itself form
duplicate groups. `sync` verifies every file before importing it.

**Hashes are cached by file stamp.** A stamp is the inode, size, and
nanosecond mtime and ctime. A file whose stamp still matches is not re-read;
this covers provider files and the store's own blobs. The stamp leaves out the
device number on purpose: macOS reassigns APFS `st_dev` at every boot, which
used to invalidate the whole cache after a restart (the first daemon pass took
9 minutes instead of under a second). Stamps are always compared for the same
path, so the device adds nothing. The `device` columns remain in the schema,
written as 0.

The cache cannot catch corruption that leaves the stamp unchanged, such as bit
rot on disk. That needs a separate check (see [Not built yet](#not-built-yet)).

**What a model file says about itself is for display only.** `modeld-formats`
reads GGUF metadata (name, architecture, quantization, size label) and the
safetensors JSON header (dominant dtype, parameter count). The parsers read
headers only, so their cost does not depend on model size. Every length field
is checked against a cap before allocating, so a corrupt file gives an error,
never a panic or an unbounded read. The facts are recorded once per artifact
and shown by `ls`; they never influence deduplication.

## Finding model files

| Source | Scanned | Left alone |
|---|---|---|
| Ollama | `blobs/`, labeled from `manifests/` | partial downloads (`-partial` names) |
| Hugging Face | `models--*/blobs/`, labeled from `snapshots/` | `.incomplete` downloads, datasets, spaces, the Xet chunk cache |
| LM Studio | `models/` | `.internal/` (app state and staging downloads), `hub/` |
| Your folders | `[scan] roots` globs in `~/.modeld/config.toml` | files not recognized as GGUF or safetensors |

The Xet directory is a transfer cache of chunks, not model storage, so it is
never scanned or modified. Each provider's on-disk layout is documented in
[`providers/`](providers/).

Scanning rules:

- **Files under 1 MiB are ignored** (configs, tokenizer JSON). `--min-size`
  changes the threshold.
- **Symlinks inside model folders are skipped**: they already share storage
  with their target.
- **Overlapping roots count each file once.** Config globs are resolved to
  real paths, and when two roots reach the same file, the first one keeps it.
  Hardlinks stay separate artifacts.
- **A scan never fails.** Anything unreadable becomes a *skip* with a reason,
  because a live machine has files being downloaded, moved, and deleted while
  it is scanned.
- **A broken config file makes the scan incomplete.** The roots it defines are
  unknown, so nothing they own may be treated as gone (see
  [References and pruning](#references-and-pruning)).

## The store

`~/.modeld` holds:

| Path | What it is |
|---|---|
| `blobs/sha256-<hex>` | One canonical copy per digest |
| `registry.db` | SQLite (WAL): artifacts, references, digest cache, clone records |
| `journal.jsonl` | Every replacement, enough to undo it |
| `lock`, `daemon.lock` | The store lock and the one-daemon lock |
| `config.toml` | Extra scan roots |
| `bin/modeld`, `daemon.log` | The daemon's own binary and its log |

**A blob is a clone of the first file seen with its digest**, so importing
costs no disk space. The blob is hashed again after cloning. Afterwards every
duplicate becomes a clone of the blob, so the store keeps a copy even when
the app that first downloaded a model deletes it.

**Imports are atomic in the registry.** The artifact row and the record that
the source file shares the blob are written in one transaction. The source is
recorded as sharing only if its stamp still equals the one it was hashed at:
a file rewritten mid-import is never mistaken for a clone of the old bytes.

### References and pruning

A reference is "this path currently holds this digest", stamped with the sync
that saw it. `ls` and `where` are built from references, and `gc` deletes blobs
that have none. So a reference may only be dropped when ModelFS can prove the
file is gone: stale references are pruned only after a *complete* pass.

A pass is incomplete when:

- a file or folder could not be read, or a file changed while being hashed;
- an import or a registry write failed;
- the config file could not be read;
- a file was deferred because it was written in the last 5 minutes.

Some skips do not count: an Ollama blob whose name is not a finished SHA-256,
an unparseable Ollama manifest, or a file on another volume. None of these can
ever hold a reference, so pruning past them loses nothing. Before this rule, a
single permanent skip disabled pruning forever.

A complete pass with no providers at all still prunes, so apps whose folders
vanished stop appearing in `ls`.

### Clone records

When a file becomes a clone of a blob, ModelFS records its path and stamp. A
file whose stamp still matches is never planned again.

Some clones predate these records, or were made by another tool. Before
replacing a file, ModelFS asks APFS whether it already shares the blob's
blocks: `fcntl(F_LOG2PHYS_EXT)` maps 16 evenly spaced offsets of both files to
physical addresses on the same device. Matching files are recorded (adopted)
instead of replaced, which avoids re-hashing gigabytes to free nothing. The
check is sampled, so a clone that diverged only between samples counts as
shared. That can only cost savings, never correctness. On the first live run,
8 of the 10 remaining candidates turned out to be clones already.

### Garbage collection

`modeld gc` deletes blobs that no reference points to, since after complete
passes zero references is a settled fact. Two guards keep a blob anyway:

1. **A journaled swap uses it.** `restore` rebuilds files from the canonical
   blob, so deleting it would break undo.
2. **A recorded clone of it still matches on disk.** The blob anchors a live
   clone that no scan currently sees, for example after a folder was removed
   from the config.

Deleting a blob never touches the bytes of its clones (copy-on-write). The
blob file goes first and its registry rows after, in one transaction, so a
failed delete leaves the artifact intact and retryable. Reported sizes are
logical; the actual space freed depends on whether other files still share
the blocks.

### Concurrency

Every command that changes files, blobs, the registry, or the journal holds
the store lock (`flock(2)` on `~/.modeld/lock`): `sync`, `dedupe`, `restore`,
`gc`, and each daemon pass. The journal is rewritten whole on every append, so
two unlocked writers could drop an entry and leave a swap impossible to undo.

Read-only commands (`ls`, `where`, `scan`, `doctor`) do not take the lock.
SQLite runs in WAL mode with a 5-second busy timeout, so a read waits out a
write transaction instead of failing.

## Replacing a duplicate

No heuristics based on mtime alone. For each file to replace (the *victim*)
and its canonical blob:

1. **Hash the canonical file** and require the expected digest.
2. **Snapshot the victim's stat**: size, mtime, mode, inode, link count.
   Refuse files written in the last 5 minutes (they may still be downloading)
   and files with more than one hard link (cloning one name would free
   nothing, since the old bytes live on under the other name).
3. **Check for open writers** with `lsof`, hash the victim, require the same
   digest, and re-stat: any change aborts.
4. **Clone the canonical file** to `.<name>.modeld-tmp-<pid>` in the victim's
   directory, which guarantees the same volume. Hash the clone, check writers
   and stat again.
5. **Swap atomically** with `renamex_np(RENAME_SWAP)`. The victim's original
   bytes now sit under the temp name, which makes rollback instant. Check the
   original inode once more; if anything raced the swap, swap back.
6. **Journal durably** (temp file, `fsync`, rename, directory `fsync`). If the
   journal write fails, swap back.
7. **Restore the victim's permissions and mtime** with nanosecond precision
   (LM Studio keys its metadata cache on millisecond mtimes), then delete the
   temp file. Deleting it is what frees the space.

`modeld restore` runs the same checks in reverse. It writes an independent
copy by reading and writing the bytes, because `std::fs::copy` would clone on
APFS and recreate the sharing that restore exists to undo. Entries whose file changed
since the replacement are left in place and kept in the journal.

Commands print only between steps, never in the middle of one. When their
output goes to a pipe whose reader has quit (`modeld ls | head -1`), they end
quietly, like other Unix tools, instead of panicking.

## The daemon

`modeld daemon` keeps the store converged. It watches every scan root with
`FSEvents` (through `notify`), plus the store directory for `config.toml`
edits, and runs *passes*: a sync followed by store-anchored consolidation.

**Events only say when to look.** `FSEvents` coalesces events and replays
sticky flags: merely hashing a file comes back as a modification. So a pass
re-derives everything from the filesystem, which makes it idempotent. The extra no-op pass after a busy one is expected and cheap.

**When a pass runs**, whichever comes first: 10 seconds after events go quiet,
2 minutes into a steady stream of events (such as a long download), when a
deferred file settles, and every 15 minutes regardless. The periodic pass
catches missed events and folders newly matched by a config glob. Each pass
re-detects roots and re-arms the watches.

**Settling.** Files written in the last 5 minutes are deferred: not imported,
and their old references are kept. Ollama and Hugging Face download under
partial names, but your own folders need not, and a half-written file must
never become a blob. `modeld sync` does not wait, because a manual command
means "now".

**Store-anchored consolidation.** After syncing, every file the registry does
not record as sharing its blob becomes a clone of it. This also covers a
case `dedupe` misses: a model re-downloaded after its original was deleted is
a single file, yet still duplicates the blob.

**Stopping.** The first SIGINT or SIGTERM stops the daemon between passes. A
pass in flight always finishes, so a swap is never cut off before it is
journaled; a pass still waiting for the store lock is abandoned. A second
signal exits immediately. A log line that cannot be written is dropped rather
than stopping a pass.

**One daemon per store.** A daemon holds `~/.modeld/daemon.lock` for its whole
lifetime. A second one, such as a terminal run while the login agent is up,
logs once and waits to take over. Exiting instead would make launchd restart
it in a loop.

### Starting at login

`modeld daemon install` writes a per-user launchd agent
(`~/Library/LaunchAgents/dev.modeld.daemon.plist`) and loads it into
`gui/<uid>`. It stops any running agent first, so running `install` again is
also how an update is applied.

- **The agent runs a copy of the binary** at `~/.modeld/bin/modeld`. Build
  directories get deleted or rebuilt, and Homebrew removes old versions. The
  copy goes through a temp file and a rename, so replacing a running binary is
  safe. It also means that after upgrading, you run `modeld daemon install`
  again.
- **The environment is written into the plist.** launchd does not inherit the
  shell's environment, so `HOME`, and `HF_HOME`, `HF_HUB_CACHE`, and
  `OLLAMA_MODELS` when set, are captured at install time. Without them the
  agent would watch different folders than the CLI.
- **Restart only on failure.** A crash or watcher failure restarts the agent;
  a clean stop does not. It runs as a background process with throttled CPU
  and I/O, so hashing a new download never competes with foreground work. It
  gets 120 seconds after SIGTERM to finish a pass before launchd kills it.
- `uninstall` stops the agent and removes the plist. The installed binary and
  the log stay.

## Building and shipping

**Rust**, edition 2024, one binary with no runtime dependencies: SQLite is
bundled, and it links only macOS system frameworks. `rust-toolchain.toml`
pins the compiler, and CI runs clippy with warnings as errors, so a new lint
on stable Rust cannot break the build unrelated to any change. CI runs on
macOS because the tests make real APFS clones.

**Releases come from tags.** Pushing `v<version>` checks that the tag matches
the crate version, runs the tests, builds Apple Silicon and Intel binaries,
merges them into one universal `modeld` with `lipo`, and publishes the
tarball and its SHA-256 as a GitHub release.

**Homebrew installs the prebuilt binary** from a personal tap
(`theGlenn/homebrew-tap`). A from-source formula would make every user install
Rust and compile for minutes, and homebrew-core requires source builds and a
track record. The linker's ad-hoc signature is enough, because Homebrew
downloads are not quarantined. `brew install theGlenn/tap/modelfs` names the
formula in full, which satisfies Homebrew's tap trust without a separate
`brew trust`. The release job updates the formula's URL and checksum through
a token scoped to the tap; without the token it warns, and the formula is
updated by hand.

## Not built yet

- **Linux**, through reflinks, and the storage trait that comes with it.
- **An opt-in hardlink mode** for filesystems without clones.
- **`doctor --verify`**, re-hashing blobs to catch corruption the stamp cache
  cannot see.
- **Log rotation** for `~/.modeld/daemon.log`.
- **Per-project drop-in files**, a `.modeld` file marking a folder for
  scanning. Config globs cover the folder layouts seen so far.
- **A FUSE view** (`fuser`) was part of the original plan; nothing depends on
  it yet.

Known gaps:

- **Stalled partial downloads outside Ollama and Hugging Face.** Your own
  folders and LM Studio's `models/` are recognized by file format, not name,
  so a download stuck for over 5 minutes under a name like `model.gguf.part`
  is imported as its own artifact.
  It never matches a complete file, so it is never swapped, and `gc` removes
  its blob once the file is gone.
- **A crash between steps 5 and 6 of a replacement** leaves the original bytes
  under the temp name, outside the journal. Nothing is lost, but the space may
  stay used and `restore` does not know about it.
- **The Intel half of the universal binary** is built but not yet tested in CI.
