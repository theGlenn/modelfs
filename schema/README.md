# modeld JSON output

`modeld --json` writes [JSON Lines](https://jsonlines.org) to stdout: one
JSON object per line. The Mac app reads it; scripts can too. The commands with
JSON output are `doctor`, `ls`, `watch`, `pass`, `rm`, and `daemon install`,
`uninstall`, and `status`. Any other command given `--json` fails with the
error code `unsupported`.

Each file in [`v1/`](v1/) is an example of one command's output. The Rust
tests check that the engine still writes exactly these shapes, and the app's
tests check that it can still read them.

## Lines

Every line has a `type` and the `schema` version:

| `type` | Payload key | Meaning |
|---|---|---|
| `event` | `event` | Progress while the command runs. Only `pass` and `rm` emit events. |
| `result` | `result` | The command's outcome. Always the last line of a run that succeeded. |
| `error` | `error` | Why the command failed: a `code` to branch on and a human `message`. |

Exit codes: `0` after a result line, `1` after an error line. `rm` writes a
result line in more cases; see [Removing](#removing).

### `pass` events

| `kind` | Fields | Meaning |
|---|---|---|
| `waiting_for_lock` | | Another modeld process (often the daemon) holds the store lock. |
| `locked` | | The pass holds the lock. |
| `hashing` | `path`, `size` | A file is being hashed. |
| `cloning` | `path`, `size` | A duplicate is being replaced with a clone. |
| `removing` | `path` | `rm` only: a step is about to run on this path. |

A caller may stop a pass with one SIGINT only before `locked`; the pass then
ends with the error code `cancelled` and changes nothing. After `locked`, the
pass ignores a single SIGINT and runs to the end. Never send a second signal:
like the daemon, modeld then exits at once, possibly between a swap and its
journal entry.

A caller that quits does not stop the pass: write errors are ignored. One
that stops reading without closing the pipe does not stall it either: events
wait in a bounded queue and are dropped when it is full. The `result` or
`error` line is written once the pass has released the store lock.

### Error codes

`store_unavailable`, `registry_unreadable`, `pass_failed`, `cancelled`,
`install_failed`, `uninstall_failed`, `unsupported`,
`confirmation_required` (`rm --json` without `--expect-plan` or `--yes`),
`remove_failed`, `watch_failed`.

## Watching a folder

`watch <folder> --json` validates an existing directory and saves its canonical
absolute path in `~/.modeld/config.toml`. Its result contains `path`, `config`
(the config file path), and `added` (`false` when already configured). It
preserves comments and other settings, writes atomically under the store lock,
and does not scan or change model files. The running daemon detects the config
change and reloads its roots. Invalid folders, unreadable or invalid configs,
and a busy store produce `watch_failed` with a recovery message.

## Removing

`rm <digest>... (--path P... | --provider APP | --everywhere)` plans first.
Its result has a `status`, the `plan`, and an `outcome` (`null` when nothing
ran):

| `status` | Exit | Meaning |
|---|---|---|
| `planned` | 0 | `--dry-run`: nothing changed. |
| `plan_changed` | 3 | The plan made under the store lock has another `plan_id` than `--expect-plan`; nothing changed. `plan` is the new one. |
| `refused` | 1 | `plan.refusals` is not empty; nothing changed. |
| `removed` | 0 | Every step ran. |
| `failed` | 1 | `outcome.failure` names the step that failed; the steps before it ran. |
| `stopped` | 1 | A signal stopped it between steps. |

A caller shows a dry-run plan, then runs it with `--expect-plan <plan_id>`,
so exactly what was shown runs. Steps run in order; the stored copy is
always last.

`plan.steps[].action`: `ollama_delete` (through Ollama's API; `tag` is set),
`unlink` (a Hugging Face snapshot link), `delete`, `delete_folder`, `trash`,
`trash_folder`, `remove_stored_copy`.

`plan.refusals[].code`: `unknown_model`, `not_referenced`,
`nothing_selected`, `outside_roots`, `not_regular_file`, `unreadable`,
`content_changed`, `in_use` (`processes` names them),
`download_in_progress`, `ollama_not_running`, `ollama_not_loopback`,
`ollama_store_mismatch`.

`plan.notes[].code`: `last_app` (the stored copy goes too),
`already_gone`, `verify_by_hashing`, `git_repository`, `symlinks`.

`plan.frees` is about how many bytes go: `now`, `after_trash` (once the
Trash is emptied), and `pinned` (bytes of removed files that other files
keep using).

## Reference states

`ls` gives each app's copy of a model a `state`:

| State | Meaning |
|---|---|
| `missing` | The file is gone; the next complete pass forgets it. |
| `unreadable` | The path could not be checked, for example because macOS denied access to its folder. It counts as a present copy. |
| `clone` | The file shares the stored copy's blocks. |
| `hardlinked` | The file has other hard links (or is the stored copy itself). ModelFS never replaces it. |
| `changed` | The file changed since ModelFS hashed it. |
| `other_volume` | An independent copy on another volume than the store. Clones cannot cross volumes, so no pass can share it. |
| `copy` | An independent copy of the stored bytes. A pass can turn it into a clone. |

`space` (per model) and `totals` are in bytes:

| Field | Meaning |
|---|---|
| `on_disk` | What the copies and stored copies occupy. Clones of one stored copy count once. |
| `saved` | What sharing saves today. |
| `pending` | What a pass could still free by cloning `copy` files. |
| `store_only` | Stored copies of models that no app uses anymore. |
| `logical` | What the apps' copies would occupy without any sharing. |

All five count files, not names: hard links to one file count once.

These are estimates. Finder and `du` count every clone at full size; free
space on the volume (`df`) is the real measure.

## Changing the format

The app bundles its own `modeld`, so app and engine always share a version.
The schema still guards against older or newer engines, such as a Homebrew
`modeld` or the daemon's copy.

- **Additive changes keep `schema: 1`**: a new field, a new command, a new
  event `kind`, a new error `code`, or a new enum value. Readers must ignore
  fields they do not know and treat unknown enum values as "unknown".
- **Breaking changes bump `schema`** and add a `v2/` folder: removing or
  renaming a field, changing a field's type or meaning, or making an optional
  field required.
- Field names are `snake_case`. Paths are absolute strings. Sizes are bytes.
  Times are RFC 3339 in UTC.

To update the examples after an intended change, run
`UPDATE_SCHEMA_FIXTURES=1 cargo test -p modeld-cli json::`, then review the
diff of `schema/v1/`.
