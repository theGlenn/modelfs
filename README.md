# ModelFS

**ModelFS keeps one copy of every local AI model on your Mac, however many apps use it.**

Ollama, LM Studio, and the Hugging Face cache each store their own copies of
model files. Download the same GGUF into all three and it takes three times
the disk space.

The `modeld` CLI finds byte-identical model files and turns the duplicates into
APFS clones. Every app keeps its files exactly where it expects them, while the
copies share the same bytes on disk.

With `modeld`, you can:

- Find model folders and report exact duplicates with `doctor` and `scan`.
- Browse models across apps and locate their files with `sync`, `ls`, and `where`.
- Preview disk savings with `dedupe --dry-run`, then consolidate with `dedupe`.
- Keep new downloads deduplicated automatically with `daemon install`.
- Add your own model folders with `watch <folder>` or the Mac app's folder picker,
  after `discover` finds the ones holding large models.
- Undo replacements with `restore`.

ModelFS is in early development. See [How it stays safe](#how-it-stays-safe).

## Install

The Mac app supports Apple Silicon and Intel on macOS 15+. Signed app releases
include `ModelFS-<version>-universal.dmg`: download it from
[GitHub Releases](https://github.com/theGlenn/modelfs/releases), open the disk
image, and drag ModelFS into Applications. App downloads and the cask become
available with the first signed app release; older CLI releases only contain
the command-line archive.

After an app release updates the Homebrew tap:

```sh
brew install --cask theGlenn/tap/modelfs
```

For the standalone CLI, on Apple Silicon or Intel Macs. From the first app
release on, the CLI ships the app's engine and also needs macOS 15+:

```sh
brew install --formula theGlenn/tap/modelfs
modeld --version
```

Or build from source with Rust 1.95 and Git:

```sh
git clone https://github.com/theGlenn/modelfs.git
cd modelfs
cargo install --locked --path crates/modeld-cli
modeld --version
```

## Quick start

1. Find model folders:

   ```sh
   modeld doctor
   ```

2. Scan for duplicates (read-only):

   ```sh
   modeld scan
   ```

   To include your own model folders, [add them to the config](#what-gets-scanned).

3. Preview, then deduplicate:

   ```sh
   modeld dedupe --dry-run
   modeld dedupe
   ```

4. Browse models across apps:

   ```sh
   modeld sync
   modeld ls
   modeld where qwen
   ```

5. Enable automatic deduplication (optional):

   ```sh
   modeld daemon install
   modeld daemon status
   ```

Example `modeld ls` output:

```text
MODEL                                        FORMAT       QUANT         SIZE  USED BY
Qwen/Qwen3.5-0.8B: model.safetensors-00001-… safetensors  BF16        1.7 GB  huggingface
Bonsai-8B                                    GGUF         -           1.2 GB  huggingface
Gemma 3 1b It                                GGUF         Q8_0        1.1 GB  manual
Qwen3.5-0.8B                                 GGUF         Q4_K_M    532.5 MB  lmstudio, manual
all-MiniLM-L6-v2                             GGUF         F16        45.9 MB  ollama

Assets (tokenizers, vocabularies):
Qwen/Qwen3.5-0.8B: tokenizer.json            -            -          12.8 MB  huggingface

Physical usage:  4.6 GB
Logical usage:   5.1 GB
Deduplicated:    532.5 MB
```

## Mac app

ModelFS also comes as a menu bar app. It shows every model across your apps,
which ones share disk space, and what deduplicating would free, and it turns
the background service on, off, or updates it.

The app includes its own engine. To build it from source, you need Rust and
Xcode 26 or later; the app needs macOS 15 or later:

```sh
scripts/build-app.sh --release
open dist/ModelFS.app
```

The app runs its own copy of `modeld` (inside `ModelFS.app/Contents/MacOS`)
and changes nothing that the CLI would not. Its first scan only adds models
to the store; **Deduplicate now** runs one `modeld pass`; **Remove…** shows
what `modeld rm` would do for each app, and runs exactly that plan.

The Models view opens in **Grouped** mode: expand a model to see its variants,
or choose **Flat** for the original table. Duplicates appear as cards with each
copy's location. **Background service** separates detected **Apps** from
expandable watched **Folders**; **Add folder…** is also in the main toolbar.

Update by replacing the app from a new DMG, or run
`brew upgrade --cask theGlenn/tap/modelfs`. If the background service needs an
update afterward, choose **Background service → Update**. The app and CLI share
one service and store. To also stop the service when removing the app, choose
**Turn off** first; model files and the store are preserved. See the
[release and installation guide](docs/releasing.md) for packaging, signing,
upgrades, and uninstall behavior.

## CLI reference

Use `modeld <command> --help` for options.

| Command | What it does | Changes files? |
|---|---|---|
| `doctor` | Lists detected model folders and excluded paths | no |
| `discover` | Suggests unwatched folders that hold large model files | no |
| `scan` | Inventories model files and reports exact duplicates | no |
| `sync` | Imports models into `~/.modeld` and indexes their metadata | store only |
| `ls` | Lists stored models, their format, quantization, size, and which apps use them | no |
| `where <query>` | Finds paths for matching models | no |
| `watch <folder>` | Adds a folder and its subfolders to the scan configuration | config only |
| `dedupe [--dry-run]` | Replaces duplicates with APFS clones | yes, undoable |
| `restore` | Rebuilds independent copies | yes |
| `gc [--dry-run]` | Deletes unused stored copies | store only |
| `rm <digest> --path P` / `--provider APP` / `--everywhere` | Removes a model from one app, or from every app and the store | yes: deletes, or moves to the Trash |
| `pass [--dry-run]` | Runs one daemon pass now: imports settled downloads, then replaces duplicates | yes, undoable; store only with `--dry-run` |
| `daemon [--dry-run]` | Runs automatic deduplication in the foreground | yes, undoable; store only with `--dry-run` |
| `daemon install` / `uninstall` / `status` | Manages the background daemon | — |

`scan`, `sync`, `dedupe`, `pass`, and `daemon` scan files of at least 1 MiB by
default, and `discover` looks for files of at least 100 MB. Use
`--min-size <bytes>` to change this.

`doctor`, `ls`, `watch`, `pass`, `rm`, and the `daemon` subcommands accept `--json`
for machine-readable output (JSON Lines). See [`schema/`](schema/README.md).

## Removing a model

`modeld where <query>` shows a model's digest and every copy. Then:

```sh
modeld rm sha256:<digest> --provider lmstudio --dry-run   # what would go
modeld rm sha256:<digest> --provider lmstudio             # asks first
modeld rm sha256:<digest> --everywhere                    # every app and the store
```

Ollama models are removed through Ollama, which must be running; Hugging Face
cache files are deleted; LM Studio, Xybrid, and your own files go to the Trash.
Removing the last copy any app has also removes the stored copy. The plan
says about how much space this frees: removing one app's copy of a model
other apps share frees about nothing.

## What gets scanned

| Source | Location |
|---|---|
| Ollama | `~/.ollama/models` (or `$OLLAMA_MODELS`) |
| Hugging Face | `~/.cache/huggingface/hub` (or `$HF_HOME`, `$HF_HUB_CACHE`) |
| LM Studio | `~/.lmstudio/models` |
| Xybrid | `~/.xybrid` (model files only; runtime caches and traces excluded) |
| Your folders | Paths listed in `~/.modeld/config.toml` |

The Hugging Face scan includes cached MLX models and excludes the Xet chunk cache.

Add a folder from Terminal, or choose **Background service → Folders → Add folder…**
in the Mac app:

```sh
modeld watch ~/models
```

The command preserves your existing configuration and avoids adding the same
folder twice. The background service picks it up automatically; if it is off,
turn it on or run `modeld pass` to import models now. Xybrid scans include
GGUF, safetensors, ONNX, and `.xyb` model bundles.

To find folders worth adding, run `modeld discover`. It looks through your
home folder for large model files that no scan covers and prints each folder
with its biggest files, the space adding it could free, and the command to
add it. A *likely copy* matches the size and sampled bytes of a stored model
or another file it found; one *already shared* is an APFS clone and frees
nothing. Folders at the same place in several
projects are suggested as one glob pattern, such as `~/games/*/Assets`, so
projects you create later are covered too. It skips `~/Library`, the Trash,
version control, `node_modules`, build output, app bundles, and files stored
only in the cloud, and it changes nothing.

You can also add folders or glob patterns to `~/.modeld/config.toml`:

```toml
[scan]
roots = [
    "~/models",
    "~/code/*/fixtures/models",
]
```

## The daemon

`modeld daemon install` starts the daemon and enables it at login. It watches
your model folders and config for changes:

- Imports downloads after 5 minutes without changes.
- Replaces duplicates with clones of stored models.
- Rescans every 15 minutes to catch missed changes.

```
$ tail ~/.modeld/daemon.log
2026-09-23T21:40:02Z settling ~/.lmstudio/models/…/model-Q4_K_M.gguf (ready in 4m 49s)
2026-09-23T21:45:03Z hashing ~/.lmstudio/models/…/model-Q4_K_M.gguf (2.6 GB)
2026-09-23T21:45:21Z cloned ~/.lmstudio/models/…/model-Q4_K_M.gguf
2026-09-23T21:45:21Z pass: 1 clone(s) made (2.6 GB freed)
```

`modeld daemon --dry-run` previews replacements. It updates the store and
registry without changing your apps' files.

After upgrading the CLI (say, with `brew upgrade --formula modelfs`), run
`modeld daemon install` again to update the daemon.

## How APFS clones work

APFS clones share disk space. Editing or deleting one copy leaves the others
unchanged. Apps keep using their existing paths without configuration changes.

## How it stays safe

- Verifies both files' full contents before replacing a duplicate.
- Skips recently modified files, files open for writing, partial downloads,
  and files with multiple hardlinks.
- Replaces files atomically and rolls back if they changed during the operation.
- Records each replacement so `modeld restore` can undo it.
- Preserves timestamps and permissions.

## Measuring the savings

Finder and `du` count each clone at full size. Compare `df -h /` before and after
deduplication to see the change in free space. Reported savings can differ if
other files still share the replaced data.

## Undoing everything

Restore needs enough free space for independent copies. Run these commands in
order, and delete `~/.modeld` only after the restore succeeds:

```sh
modeld daemon uninstall   # stop the background agent
modeld restore           # rebuild independent copies
rm -rf ~/.modeld          # remove ModelFS data
```

## Limitations

- Requires APFS. Models on a different volume from `~/.modeld` are skipped.
- Different quantizations or formats are not deduplicated.
- Linux support is planned.
- The daemon log is not yet rotated.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets
cargo fmt --all
```

To release, bump `version` in `Cargo.toml`, merge it, then push a matching
tag from `master`:

```sh
git tag v0.2.0
git push origin v0.2.0
```

The release workflow validates the tag, tests the engine and app, and builds
universal macOS artifacts in a job without signing credentials. A separate job
signs, notarizes, and staples the app and DMG before publishing the DMG and CLI
archive with checksums on GitHub Releases.
The CLI archive reuses the app's signed engine. A follow-up job updates both
the Homebrew formula and app cask. Release credentials and the first-release
checklist are documented in [docs/releasing.md](docs/releasing.md).

See [`DECISIONS.md`](DECISIONS.md) for design decisions and [`providers/`](providers/)
for notes on how each app stores its models.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
