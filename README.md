# modeld

**One copy of every local AI model on your Mac, however many apps use it.**

Ollama, LM Studio, the Hugging Face cache, and every project's `fixtures/models`
folder each keep their own copy of the model files they download. Pull the same
GGUF into three of them and it takes three times the disk. `modeld` finds
byte-identical model files and turns the duplicates into APFS clones: every app
keeps its files exactly where it expects them, and the bytes live on disk once.

```
$ modeld ls
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

Status: early and macOS-only, running on its author's Mac. The safety protocol is
deliberately paranoid (see [How it stays safe](#how-it-stays-safe)), and every
change it makes can be undone with `modeld restore`.

## Why clones, not hardlinks or symlinks

An APFS clone (`clonefile(2)`) shares the underlying disk blocks, so it costs
the same as a hardlink, but each path keeps its own inode. If an app later
rewrites its copy, copy-on-write gives it private blocks; the other copies
never change. Apps cannot tell a clone from the file they downloaded, so no
plugins, config changes, or symlinks are needed.

## Requirements

- macOS, with your models on an APFS volume (the default for the system disk)
- A recent stable Rust toolchain (edition 2024; developed on Rust 1.95)

## Install

```sh
cargo install --path crates/modeld-cli   # puts `modeld` in ~/.cargo/bin
```

## Quick start

1. See which model folders modeld found:

   ```sh
   modeld doctor
   ```

2. Get a read-only duplicate report (changes nothing):

   ```sh
   modeld scan
   ```

3. Preview, then consolidate duplicates:

   ```sh
   modeld dedupe --dry-run
   modeld dedupe
   ```

4. Build the global view, then browse it:

   ```sh
   modeld sync
   modeld ls
   modeld where qwen
   ```

5. Let the daemon keep things deduplicated from now on, starting at every
   login:

   ```sh
   modeld daemon install
   modeld daemon status
   ```

## Commands

| Command | What it does | Changes files? |
|---|---|---|
| `doctor` | Lists detected model folders and what is excluded | no |
| `scan` | Inventories model files and reports exact duplicates | no |
| `sync` | Registers every model in the store (`~/.modeld`) and reads model facts from file headers | store only |
| `ls` | Lists stored models, their format, quantization, size, and which apps use them | no |
| `where <query>` | Shows the stored copy and every path using models matching `<query>` | no |
| `dedupe [--dry-run]` | Replaces byte-identical duplicates with clones of the stored copy | yes, undoable |
| `restore` | Undoes every replacement, rebuilding fully independent copies | yes |
| `gc [--dry-run]` | Deletes stored copies that nothing uses anymore | store only |
| `daemon [--dry-run]` | Watches model folders in the foreground and keeps them deduplicated | yes, undoable |
| `daemon install` / `uninstall` / `status` | Manages the login agent that runs the daemon in the background | — |

## What gets scanned

| Source | Location | Notes |
|---|---|---|
| Ollama | `~/.ollama/models` (or `$OLLAMA_MODELS`) | Blob names are SHA-256 digests |
| Hugging Face | `~/.cache/huggingface/hub` (or `$HF_HOME`, `$HF_HUB_CACHE`) | The Xet chunk cache is excluded and never touched; MLX models live here too |
| LM Studio | `~/.lmstudio/models` | Loose files; hashed on demand |
| Your folders | Anything listed in `~/.modeld/config.toml` | Shown as `manual` |

Add your own folders, including glob patterns, in `~/.modeld/config.toml`:

```toml
[scan]
roots = [
    "~/models",
    "~/code/*/fixtures/models",
]
```

Symlinked aliases of the same folder are only counted once, and nested roots
never make a file look like its own duplicate. Only files of 1 MiB or more are
considered (`--min-size` changes that).

## The daemon

`modeld daemon install` sets up a launchd login agent that runs
`modeld daemon` in the background at low priority. It watches every scanned
folder, plus `~/.modeld/config.toml`, for changes:

- A new download is imported once it has stopped changing for 5 minutes, so
  half-finished files are never stored.
- A file that duplicates a stored model (a second app downloading it, or a
  re-download after you deleted it) becomes a clone of the stored copy.
- It also checks everything every 15 minutes, in case it missed an event or
  a new project folder matched a pattern in your config.

```
$ tail ~/.modeld/daemon.log
2026-09-23T21:40:02Z settling ~/.lmstudio/models/…/model-Q4_K_M.gguf (ready in 4m 49s)
2026-09-23T21:45:03Z hashing ~/.lmstudio/models/…/model-Q4_K_M.gguf (2.6 GB)
2026-09-23T21:45:21Z cloned ~/.lmstudio/models/…/model-Q4_K_M.gguf
2026-09-23T21:45:21Z pass: 1 clone(s) made (2.6 GB freed)
```

To see what it would do without letting it change anything, run
`modeld daemon --dry-run` in a terminal. The agent runs its own copy of the
binary (`~/.modeld/bin/modeld`), so after upgrading modeld, run
`modeld daemon install` again. Only one daemon runs at a time: a second one
waits and takes over when the first exits.

## How it stays safe

Replacing a file that an app owns is the risky part, so each replacement:

1. **Hashes both files in full** and proceeds only when the SHA-256 digests
   match. Digests read from file names are treated as hints, never as proof.
2. **Leaves busy files alone.** Files written in the last 5 minutes, files
   another process has open for writing (checked with `lsof`), partial
   downloads, and files with several hardlinks are all skipped.
3. **Swaps atomically.** The clone is built next to the original, verified,
   and exchanged with it in one step (`renamex_np` with `RENAME_SWAP`). If
   anything changed in the meantime, the swap is rolled back.
4. **Journals before releasing space.** Each swap is written to
   `~/.modeld/journal.jsonl` before the original bytes are freed, so
   `modeld restore` can always rebuild an independent copy.
5. **Preserves timestamps and permissions.** LM Studio, for example, keys its
   caches on a file's modification time.

The daemon and the commands that change files take turns through a lock, so a
manual `dedupe` never races a daemon pass.

## Measuring the savings

Finder and `du` report every clone at full size, because each one does
contain the whole file. To see the real effect, compare free space on the
volume (`df -h /`) before and after. Sizes reported as "freed" are the bytes
modeld stopped storing twice; the disk actually gets them back only once no
other file still shares those blocks.

## Undoing everything

Run these in this order: the journal in `~/.modeld` is what `restore` needs.

```sh
modeld daemon uninstall   # stop the background agent
modeld restore            # rebuild independent copies; needs the space back
rm -rf ~/.modeld          # remove the store, registry, journal, and log
```

Deleting the store never touches the files your apps use: clones keep their
data after the original is gone.

## Where things live

```
~/.modeld/
├── blobs/           one stored copy per unique model (sha256-<hex>)
├── registry.db      SQLite: models, which paths use them, cached digests
├── journal.jsonl    every replacement, for `modeld restore`
├── config.toml      your extra scan folders (optional)
├── daemon.log       what the daemon did
└── bin/modeld       the copy the login agent runs
~/Library/LaunchAgents/dev.modeld.daemon.plist
```

## Limitations

- macOS and APFS only. Linux support (reflinks on Btrfs/XFS) is planned but
  not built.
- Clones only work within one volume, so models on an external drive are
  skipped.
- Only byte-identical files are deduplicated. Different quantizations or
  formats of the same model are separate files.
- The daemon's log is not rotated yet.

## Development

```sh
cargo test --workspace
cargo clippy --workspace --all-targets   # pedantic lints; kept at zero warnings
cargo fmt --all
```

| Crate | Responsibility |
|---|---|
| `modeld-core` | Artifacts, digests, duplicate detection, APFS syscalls, the replacement protocol and journal |
| `modeld-providers` | Finding model folders and scanning them (Ollama, Hugging Face, LM Studio, config roots) |
| `modeld-formats` | Bounded GGUF and safetensors header parsers |
| `modeld-store` | Content-addressed blob store, SQLite registry, store lock |
| `modeld-cli` | The `modeld` binary: commands, the sync step, the daemon, the launchd agent |

The reasoning behind the design lives in [`DECISIONS.md`](DECISIONS.md), and
notes on how each app stores its models are in [`providers/`](providers/).

## License

Dual-licensed under MIT or Apache-2.0, at your option.
