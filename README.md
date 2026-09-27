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
- Undo replacements with `restore`.

ModelFS is in early development. See [How it stays safe](#how-it-stays-safe).

## Install

With Homebrew, on Apple Silicon or Intel Macs:

```sh
brew install theGlenn/tap/modelfs
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

## CLI reference

Use `modeld <command> --help` for options.

| Command | What it does | Changes files? |
|---|---|---|
| `doctor` | Lists detected model folders and excluded paths | no |
| `scan` | Inventories model files and reports exact duplicates | no |
| `sync` | Imports models into `~/.modeld` and indexes their metadata | store only |
| `ls` | Lists stored models, their format, quantization, size, and which apps use them | no |
| `where <query>` | Finds paths for matching models | no |
| `dedupe [--dry-run]` | Replaces duplicates with APFS clones | yes, undoable |
| `restore` | Rebuilds independent copies | yes |
| `gc [--dry-run]` | Deletes unused stored copies | store only |
| `daemon [--dry-run]` | Runs automatic deduplication in the foreground | yes, undoable; store only with `--dry-run` |
| `daemon install` / `uninstall` / `status` | Manages the background daemon | — |

`scan`, `sync`, `dedupe`, and `daemon` scan files of at least 1 MiB by default.
Use `--min-size <bytes>` to change this.

## What gets scanned

| Source | Location |
|---|---|
| Ollama | `~/.ollama/models` (or `$OLLAMA_MODELS`) |
| Hugging Face | `~/.cache/huggingface/hub` (or `$HF_HOME`, `$HF_HUB_CACHE`) |
| LM Studio | `~/.lmstudio/models` |
| Your folders | Paths listed in `~/.modeld/config.toml` |

The Hugging Face scan includes cached MLX models and excludes the Xet chunk cache.

Add folders or glob patterns to `~/.modeld/config.toml`:

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

After upgrading the CLI (say, with `brew upgrade modelfs`), run
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

The release workflow tests the tag, publishes a universal macOS binary on
GitHub Releases, and updates the Homebrew formula.

See [`DECISIONS.md`](DECISIONS.md) for design decisions and [`providers/`](providers/)
for notes on how each app stores its models.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
