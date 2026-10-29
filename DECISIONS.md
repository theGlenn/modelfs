# Design decisions

ModelFS saves disk space by replacing duplicate model files with APFS clones.
This document explains the safety checks, trade-offs, and limits behind that
design. The [README](README.md) covers installation and use.

When a change alters a choice described here, update this document in the
same PR. The dated log this document replaced is in the git history.

## Principles

Every choice below follows from four rules, in priority order:

1. **Never lose or corrupt a file.** When ModelFS is unsure, it skips. A missed
   saving costs disk space; a wrong replacement costs someone's data.
2. **Apps keep their paths.** When ModelFS replaces a duplicate, it never
   moves the file or turns it into a symlink: a clone with the same bytes,
   permissions, and modification time takes its place. Only removing a model,
   which you ask for model by model, deletes files or moves them to the
   Trash (see [Removing a model](#removing-a-model)).
3. **Replacements must be undoable.** Completed replacements are journaled,
   and `modeld restore` rebuilds independent copies. The crash window before
   journaling is listed under [Known gaps](#known-gaps).
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

A file's identity is the SHA-256 of its full contents. SHA-256 is what the
ecosystem already uses: Ollama and Hugging Face LFS name their blobs by it.
The `Digest` type carries its algorithm (`sha256:<hex>`), so a faster hash or
a chunk-level scheme can be added without changing the schema.

A filename can claim a digest without proving it. Two files can claim the same
hash, and a Hugging Face 40-hex name is a git SHA-1 that never matches the SHA-256 of an
identical copy. Harvested names only save work: a file whose size nothing else
shares cannot have a duplicate, so `scan` and `dedupe` do not hash it. Files
with colliding sizes are hashed, and only digests ModelFS computed itself form
duplicate groups. `sync` verifies every file before importing it.

Hashes are cached by file stamp: the inode, size, and nanosecond mtime and ctime.
A file whose stamp still matches is not re-read;
this covers provider files and the store's own blobs. The stamp leaves out the
device number on purpose: macOS reassigns APFS `st_dev` at every boot, which
used to invalidate the whole cache after a restart (the first daemon pass took
9 minutes instead of under a second). Stamps are always compared for the same
path, so the device adds nothing. The `device` columns remain in the schema,
written as 0.

The cache cannot catch corruption that leaves the stamp unchanged, such as bit
rot on disk. That needs a separate check (see [Not built yet](#not-built-yet)).

Model metadata is for display only. `modeld-formats` reads GGUF metadata
(name, architecture, quantization, size label) and the safetensors JSON header
(dominant dtype, parameter count). The parsers read
headers only, so their cost does not depend on model size. Every length field
is checked against a cap before allocating, so a corrupt file gives an error,
never a panic or an unbounded read. The facts are recorded once per artifact
and shown by `ls`; they never influence deduplication. Display names replace
generic header names (`Hf`, `Lm`, `model`, `unknown`, `unnamed`, or blank) with
descriptive filenames, provider-qualified labels, or a short digest. This happens
when reading, so existing indexed models benefit without rewriting metadata.
Quantization is never inferred from the filename.

## Finding model files

| Source | Scanned | Left alone |
|---|---|---|
| Ollama | `blobs/`, labeled from `manifests/` | partial downloads (`-partial` names) |
| Hugging Face | `models--*/blobs/`, labeled from `snapshots/` | `.incomplete` downloads, datasets, spaces, the Xet chunk cache |
| LM Studio | `models/` | `.internal/` (app state and staging downloads), `hub/` |
| Xybrid | Model files under `~/.xybrid`, including `.xyb` bundles | `cache/precompiled`, `cache/xcframework`, `cache/ort-ios`, `traces`, sidecars |
| Your folders | `[scan] roots` globs in `~/.modeld/config.toml` | files not recognized as model formats |

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
- **The store is never scanned as one of your folders.** A broad glob such
  as `~/*` would otherwise record the store's own blobs as copies.
- **App exclusions also apply to overlapping folders.** Watching a broad parent
  folder cannot re-import Xet chunks, app staging folders, Xybrid runtime caches,
  or traces through a user-configured root.
- **Unreadable files do not stop a scan.** Each becomes a *skip* with a reason,
  because a live machine has files being downloaded, moved, and deleted while
  it is scanned.
- **A broken config file makes the scan incomplete.** The roots it defines are
  unknown, so nothing they own may be treated as gone (see
  [References and pruning](#references-and-pruning)).

### Suggesting folders

Model files often live in project folders no provider knows about: game
assets, test fixtures, agent scratch folders. `modeld discover` walks the home
folder once and suggests folders to add. It only suggests: adding a folder is
always the user's choice, because the daemon then replaces copies in it.

- **Its own walk, not Spotlight.** On the machine this was built on, Spotlight
  found 15 of 76 large model files: it skips hidden folders such as
  `.context` and `.cache` and does not index every model format.
- **Skipped while walking:** folders already scanned and the apps' protected
  subtrees; `~/Library` (app data behind privacy prompts, and cloud storage);
  the Trash and the store; `.git`, `node_modules`, `target`, `DerivedData`,
  `.build`, and `Pods`; app and framework bundles, whose copies are build
  output; other volumes, which clones cannot reach; and files and folders
  stored only in the cloud (`SF_DATALESS`), since reading them downloads them.
- **Files of at least 100 MB a scan would record.** `.bin` and `.pt` must also
  start like weights (GGUF, GGML, a PyTorch zip or pickle). Other tools use
  these extensions too, such as Unity's `Library/DataStore`.
- **Copies are estimated from samples, not hashed.** Hashing every candidate
  would take minutes per gigabyte, and a pass hashes before it replaces
  anything. Size alone misleads: fine-tunes sharing an architecture and
  quantization have the same byte size, and on the first real run two of
  three same-size Gemma files held different weights. So a *likely copy*
  matches the size and sixteen 4 KiB samples spread over a stored model or
  another candidate.
- **Only unshared bytes count as savings.** Copies made in Finder or with
  `cp -c` are already APFS clones. A likely copy counts its private size
  (`ATTR_CMNEXT_PRIVATESIZE`), so a clone shows as *already shared* and adds
  nothing. Suggestions that would free the most come first.
- **Suggested folders stay inside projects.** A project root is a folder with
  `.git` (a repository or worktree) or Unity's `ProjectSettings`. A
  suggestion is the deepest folder holding a group of files, but never the
  home folder, a project root, or a folder containing a project: scanning
  those would record build output and caches as models. Files directly in
  such a folder are listed as not suggested.
- **Sibling projects become a pattern.** When the same subfolder of several
  projects in one parent holds models, the suggestion is a glob pattern
  (`~/games/*/Assets`), so projects created later are covered. Names are
  escaped, and projects directly in the home folder are not merged: `~/*/Assets`
  would match far more than those projects.
- **Text only.** No app runs `discover` yet, so it has no JSON output (see
  [Machine-readable output](#machine-readable-output)).

## The store

`~/.modeld` holds:

| Path | What it is |
|---|---|
| `blobs/sha256-<hex>` | One canonical copy per digest |
| `registry.db` | SQLite (WAL): artifacts, references, digest cache, clone records |
| `journal.jsonl` | Every replacement, enough to undo it |
| `lock`, `daemon.lock` | The store lock and the one-daemon lock |
| `config.toml` | Extra scan roots |
| `generation` | A counter every change bumps; viewers watch it |
| `bin/modeld`, `daemon.log` | The daemon's own binary and its log |

A blob is a clone of the first file seen with its digest, so importing
does not duplicate its data blocks. The blob is hashed again after cloning.
Later replacements clone from the blob, so the store keeps a copy even when
the app that first downloaded a model deletes it.

Imports are atomic in the registry. The artifact row and the record that
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
ever hold a reference, so they do not block pruning.

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
shared. A missed difference leaves the file untouched; it only costs savings.

### Garbage collection

`modeld gc` deletes blobs with no references, subject to two guards:

1. **A journaled swap uses it.** The blob is kept while a journal entry names
   it, even though `restore` currently copies the file at the replaced path.
   `rm --everywhere` deletes the blob regardless: restoring a remaining
   entry still works, because it copies the replaced file itself.
2. **A recorded clone of it still matches on disk.** The blob anchors a live
   clone that no scan currently sees, for example after a folder was removed
   from the config.

Deleting a blob never touches the bytes of its clones (copy-on-write). The
blob file is deleted first, then its registry rows are removed in one database
transaction. A failed file deletion leaves the registry intact for a retry.
Reported sizes are logical; the actual space freed depends on whether other
files still share the blocks.

### Concurrency

Every command that changes files, blobs, the registry, or the journal holds
the store lock (`flock(2)` on `~/.modeld/lock`): `sync`, `dedupe`, `restore`,
`gc`, `pass`, `watch`, and each daemon pass. The journal is rewritten whole on every
append, so two unlocked writers could drop an entry and leave a swap
impossible to undo.

When one of them finishes, it rewrites `~/.modeld/generation` so viewers know
to refresh (see [Machine-readable output](#machine-readable-output)). The
daemon ignores events in the store directory, so this write never wakes it.

Read-only commands (`ls`, `where`, `scan`, `doctor`) do not take the lock.
SQLite runs in WAL mode with a 5-second busy timeout, so a read waits out a
write transaction instead of failing.

## Replacing a duplicate

Replacement requires matching content hashes and checks for concurrent writes.
For each file to replace (the *victim*) and its canonical blob:

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

`modeld restore` verifies the current file against its journaled digest, then
reads and writes its bytes to an independent copy. `std::fs::copy` would clone
on APFS and recreate the sharing that restore exists to undo. Restore checks
for concurrent writes before and after swapping the copy into place. Files
whose contents changed since replacement are left untouched, and their entries
stay in the journal.

Commands do not print during a replacement. When their output goes to a pipe
whose reader has quit (`modeld ls | head -1`), they end
quietly, like other Unix tools, instead of panicking.

## The daemon

`modeld daemon` keeps the store in sync with model folders. It watches every
scan root with `FSEvents` (through `notify`), plus the store directory for `config.toml`
edits, and runs *passes*: a sync followed by store-anchored consolidation.

**Events only say when to look.** `FSEvents` coalesces events and replays
sticky flags: merely hashing a file comes back as a modification. So a pass
checks the filesystem again instead of treating each event as a change to apply.
The extra pass after a busy one usually finds nothing to do.

A pass runs at the earliest of four triggers: 10 seconds after events go quiet,
2 minutes into a steady stream of events (such as a long download), when a
deferred file settles, or the 15-minute periodic check. The periodic pass
catches missed events and folders newly matched by a config glob. Each pass
re-detects roots and re-arms the watches.

**Settling.** Files written in the last 5 minutes are deferred: not imported,
and their old references are kept. Ollama and Hugging Face download under
partial names, but your own folders need not. The delay reduces the chance of
importing a half-written file; it cannot prove a download has finished.
`modeld sync` imports immediately without this settling delay.

**Store-anchored consolidation.** After syncing, files not known to share their
blob's blocks are considered for replacement, subject to the safety checks
above. This also covers a case `dedupe` misses: a model re-downloaded after its original was deleted is
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
  and I/O to reduce competition with foreground work. It gets 120 seconds after
  SIGTERM to finish a pass before launchd kills it.
- `uninstall` stops the agent and removes the plist. The installed binary and
  the log stay.

## Removing a model

`modeld rm` removes a model from one app (`--path`, `--provider`) or from
every app and the store (`--everywhere`). Removing the last copy any app has
also removes the stored copy, since nothing would use it.

**Plan, then run exactly the plan.** A dry run returns the plan: its steps,
what blocks it (refusals), notes, other files that go with it, and about how
much space it frees. Its `plan_id` hashes the steps, the refusals, every
touched file's stamp, the Ollama manifests, the Hugging Face links, and the
model's references. Running takes the store lock, plans again, and with
`--expect-plan` refuses to run a plan whose id changed (exit 3). A plan with
refusals never runs. Steps stop at the first failure, or between steps on
SIGINT, and the stored copy is always the last step, so a partial failure
keeps it.

**Every path is checked.** It must be a reference the registry knows, inside
a folder modeld scans now for that app (never inside the store), laid out
the way that app lays out files, a regular file, and still hold the model's
bytes: a cached digest for its current stamp, or a hash under the lock. No
process may have it open (`lsof`); a loaded model is mapped into memory, and
Ollama keeps one mapped while it is loaded.

**And checked again before each step.** Steps run one after another, and an
Ollama delete can take seconds. Each step carries what it relies on: its
files keep their stamps and stay closed, Ollama still lists the tag with the
same manifest, an orphan blob is still named by no manifest. If a fact no
longer holds, that step and every later one do not run.

**A file on an unplugged drive is not gone.** A missing file counts as gone
only when its volume is mounted. A copy on a disconnected drive under
`/Volumes` still counts as an app's copy: removing every other copy does not
remove the stored copy, and removing it waits until the drive is back.

**Each app's files go the way that app expects.**

| App | How | Why |
|---|---|---|
| Ollama | `DELETE /api/delete` for every tag using the blob | Tags share blobs; Ollama deletes the blobs no remaining tag uses, so its manifests and blobs never disagree. |
| Hugging Face | Unlink snapshot links, then delete the blob | The hub cache is a download cache. Links go first so none dangles; `snapshots/<commit>` and `refs/` stay. |
| LM Studio | Move the file to the Trash | The folder goes too once no GGUF or safetensors file would remain. `.internal/` is never touched. |
| Your folders | Move the file to the Trash | Notes say when the file is inside a git repository or other symlinks point at it. |

For Ollama, the API must be on this machine (`OLLAMA_HOST` loopback, or
unspecified), answer `GET /api/tags` within a second, and list each tag with
the SHA-256 of the manifest file modeld read (that is what its `digest`
field is). Otherwise the running Ollama uses another models folder, and the
removal is refused. `DELETE /api/delete` removes the tag's manifest and the
tag's own blobs that no other tag uses; tested on Ollama 0.40.4, it left an
unrelated orphan blob and a `-partial` file alone. A blob no manifest names
is deleted directly, unless a `-partial` download of it exists, it changed
in the last 5 minutes, or any manifest could not be read (that one might
name it). With Ollama closed, the removal waits for you to open it; there is
no offline fallback, because only Ollama knows which of its blobs are still
in use.

For Hugging Face, nothing is touched while `huggingface_hub` holds the
blob's download lock (`.locks/<repo>/<etag>.lock`) or an `.incomplete` file
exists, and modeld holds the lock itself while deleting. The repo folder
(`models--<org>--<name>`, directly in the hub folder) is deleted once no
blob of 1 MiB or more, or still downloading, would remain; everything else
in it is listed as going along. An LM Studio or Hugging Face folder that
cannot be read in full stays, since what cannot be read might be a model.
The plan id covers every file that goes along, so a file appearing in such
a folder after the dry run changes the plan.

**Freed space is an estimate.** Files sharing blocks form a group: the stored
copy with its clones, or the names of one hard-linked inode. A group's
blocks are freed only when every member goes, and once the Trash is emptied
if a member was trashed. When only some members go, each removed file frees
its private bytes (`getattrlist(ATTR_CMNEXT_PRIVATESIZE)`, exact per file),
and the rest stays pinned. APFS clone IDs (`ATTR_CMNEXT_CLONEID`) cannot
group files: a clone gets a new ID as soon as one block diverges, while it
still shares the rest. Removing one app's clone of a shared model therefore
frees about nothing, and the app says so.

**Afterwards** the registry forgets every removed path, files that went
along included (an Ollama layer can be another model's reference), and the
journal drops entries whose replaced file is gone, so `restore` and `gc` are
not blocked by files that no longer exist.

`/usr/bin/trash` moves a file to the Trash under its own name, so it can be
dragged back. Whether Finder's **Put Back** works for it is not verified:
macOS privacy controls kept the check from reading the Trash.

## Machine-readable output

The Mac app runs the `modeld` binary bundled inside it and reads its `--json`
output, instead of linking the Rust code. The safety checks, the store lock,
and the journal stay in one place, shared by the CLI, the daemon, and the
app. Quitting the app cannot cut a swap short: the engine is its own process
and keeps running.

- **JSON Lines with a version.** Every line has a `type` (`event`, `result`,
  or `error`) and `schema: 1`. Adding a field keeps the version; removing or
  changing one bumps it. [`schema/README.md`](schema/README.md) has the rules,
  and `schema/v1/` has example output that the Rust tests compare against and
  the app's tests decode.
- **Only what the app runs.** `doctor`, `ls`, `watch`, `pass`, `rm`, and `daemon
  install`, `uninstall`, and `status` have JSON output. Other commands
  refuse `--json`. Text output is unchanged.
- **A reader that quits never stops a pass.** With `--json` (and for every
  `pass`), `SIGPIPE` stays ignored and write errors are dropped, as in the
  daemon's log. Events go through a bounded queue on their own thread, so a
  reader that stops reading cannot stall a pass that holds the store lock.
  One SIGINT stops a pass only while it waits for the lock, before its
  `locked` event. After that, the pass finishes and journals every swap; a
  second signal still exits at once, as for the daemon.
- **`pass` is a daemon pass on demand**, with the same 5-minute settle window.
  `pass --dry-run` fills the store without changing any app's files.
- **Totals come from each copy's state.** `ls --json` checks every referenced
  path on disk: `missing`, `unreadable`, `clone` (shares the stored copy's
  blocks), `hardlinked`, `changed` (rewritten since it was hashed),
  `other_volume`, or `copy` (an independent copy a pass can clone). Only
  clones count as saved, and only copies on the store's volume as pending.
  Hard links count once. A path macOS will not let modeld check is
  `unreadable`, never `missing`: "missing" would claim the file is gone. The
  text `ls` footer still subtracts physical from logical size, which counts
  every reference as shared before any clone exists.
- **Viewers watch one file.** `sync`, `dedupe`, `restore`, `gc`, and every
  pass rewrite `~/.modeld/generation` through a temp file and a rename.
  Watching the registry would loop: SQLite creates and deletes
  `registry.db-wal` on reads too, so each refresh would trigger the next.

## The Mac app

A SwiftUI menu bar app (`app/`, a Swift package) shows the inventory and runs
passes. It is a viewer and a remote control for the engine; every decision
about files stays in `modeld`.

Models default to a grouped table of named variants, with a persisted Flat
option. Each leaf keeps its digest and per-model removal plan; a group is a
view summary, never a removal target. Duplicates use one card per digest.
Background service lists known apps separately from expandable watched folders.
The folder picker runs `modeld watch`: it only updates configuration, then
offers an explicit scan. Adding a folder never starts deduplication itself.

- **The engine is bundled and run as a subprocess.** `scripts/build-app.sh`
  puts the `modeld` it builds at `ModelFS.app/Contents/MacOS/modeld`, so the
  app and its engine share a version and a JSON schema. Debug builds may
  point `MODELD_BIN` at a `cargo build` output instead.
- **The folder variables match the daemon's.** A Mac app does not inherit the
  shell's environment, and a pass with the wrong `HF_HOME` would scan the
  wrong folders and prune every reference under the right ones. The app reads
  `HF_HOME`, `HF_HUB_CACHE`, and `OLLAMA_MODELS` from the installed agent's
  plist first, otherwise from a login shell (`-l -i`, 5-second limit), and
  passes them, with `HOME` and a fixed `PATH`, to every engine run, including
  `daemon install`. If neither can be read, the app still shows the
  inventory but runs no pass and installs nothing until a retry succeeds.
- **Each pipe is read on its own thread, to the end.** Foundation's
  `FileHandle.bytes` reads every handle on one shared serial queue: a blocking
  read of stderr kept stdout from being read, and the engine, blocked on a
  full stdout pipe, never exited. The app reads stdout and stderr with
  `read(2)` on one thread each, and keeps reading after the caller stops
  waiting.
- **Cancelling is one SIGINT before `locked`, at most.** The app shows Cancel
  only while a pass waits for the store lock and never signals twice.
- **Removing shows the plan that runs.** The Remove sheet fetches dry-run
  plans in parallel, one per app holding a copy plus everywhere (one option
  when a single app holds it), with what each frees, its steps by app, what
  blocks it with a way to fix it, and notes. Confirming passes the plan's id
  (`--expect-plan`); if anything changed, nothing is removed and the sheet
  shows the new plan. A started removal cannot be cancelled.
- **One change at a time.** A pass, installing, or removing the service is
  owned by the app's model, not by a view, so closing the window never cuts
  one short. Quitting the app leaves a running engine to finish.
- **Refreshes come from four files.** FSEvents on `~/.modeld` wakes the app
  for `generation`, `daemon.log`, `journal.jsonl`, and `config.toml`;
  requests within 250 ms run once, and requests during a refresh run once
  more after it. Refreshes never overlap, so an older one cannot overwrite a
  newer one, and a change shows as finished only once its results do.
- **No sandbox.** The app runs `modeld`, which reads app folders anywhere in
  your home folder and runs `launchctl` and `lsof`.
- **Menu bar first.** The app has no Dock icon until its window opens. The
  window opens by itself only while the store is empty, and when the app is
  launched again while running.

## Building and shipping

ModelFS uses Rust, edition 2024, and ships as one binary. SQLite is bundled;
the binary links only macOS system libraries and frameworks. `rust-toolchain.toml`
pins the compiler, and CI runs clippy with warnings as errors, so a new lint
on stable Rust cannot break the build unrelated to any change. CI runs on
macOS because the tests make real APFS clones, and runs the tests on both an
Apple Silicon and an Intel runner because releases ship both.

**Releases come from stable version tags.** Pushing `v<version>` checks that
the tag matches the crate version and points at a commit included in `master`,
runs the Rust and Swift tests, and builds one universal app for Apple Silicon
and Intel. A temporary keychain supplies the Developer ID Application identity;
the nested engine and app are signed with hardened runtime and timestamps.
The app is notarized and stapled before packaging; the DMG is then signed,
notarized, stapled, and verified. The workflow publishes only after these
steps succeed. Missing signing credentials fail the release rather than
publishing a test artifact, and a tag that already has a release or draft fails
before building.

**Release credentials never meet build code.** Compiling and testing run every
dependency's build scripts and tests; a separate job does that without secrets
and hands over the unsigned app. The signing job signs those binaries and never
compiles or runs them. Separate steps of one job would not be enough: a step can
leave a process running, or change the environment of every later step.

**The app and CLI ship the same engine bytes.** The CLI archive copies the
already signed engine from the app. Rebuilding or signing it separately would
change its bytes and cause the service's binary comparison to report an update
even at the same version. The DMG and CLI archive receive checksums after their
contents, signatures, and notarization tickets are final.

**Homebrew has two installation routes** in the personal tap
(`theGlenn/homebrew-tap`): the existing `modelfs` formula installs the CLI;
`brew install --cask theGlenn/tap/modelfs` installs the app. The cask does not
link a second `modeld` into PATH or automatically install a service. The release
job updates both URLs and checksums with a token scoped to the tap. Because the
CLI archive carries the app's engine, built for macOS 15, the job also pins the
formula to macOS 15; older Macs keep the last CLI release that ran there.

**Service lifetime is independent of app installation.** Replacing or removing
the app preserves the shared service and its environment. The app offers the
existing explicit service Update/Turn off controls; the cask's explicit zap
also removes the service. Store blobs and provider files are never part of
cask cleanup. Initial app updates use a new download or Homebrew upgrade;
an in-app updater can be added later. See [docs/releasing.md](docs/releasing.md)
for credentials, packaging commands, and release smoke tests.

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

## Known gaps

- **Stalled partial downloads outside Ollama and Hugging Face.** Your own
  folders and LM Studio's `models/` are recognized by file format, not name,
  so a download stuck for over 5 minutes under a name like `model.gguf.part`
  can be imported as its own artifact. Replacement still requires matching
  hashes and the usual safety checks; the filename does not establish whether
  a download is complete. Its blob becomes eligible for `gc` once its reference
  is pruned, subject to the journal and clone guards.
- **A crash between steps 5 and 6 of a replacement** leaves the original bytes
  under the temp name, outside the journal. Nothing is lost, but the space may
  stay used and `restore` does not know about it.
