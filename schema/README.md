# modeld JSON output

`modeld --json` writes [JSON Lines](https://jsonlines.org) to stdout: one
JSON object per line. The Mac app reads it; scripts can too. The commands with
JSON output are `doctor`, `ls`, `pass`, and `daemon install`, `uninstall`,
and `status`. Any other command given `--json` fails with the error code
`unsupported`.

Each file in [`v1/`](v1/) is an example of one command's output. The Rust
tests check that the engine still writes exactly these shapes, and the app's
tests check that it can still read them.

## Lines

Every line has a `type` and the `schema` version:

| `type` | Payload key | Meaning |
|---|---|---|
| `event` | `event` | Progress while the command runs. Only `pass` emits events. |
| `result` | `result` | The command's outcome. Always the last line of a run that succeeded. |
| `error` | `error` | Why the command failed: a `code` to branch on and a human `message`. |

Exit codes: `0` after a result line, `1` after an error line.

### `pass` events

| `kind` | Fields | Meaning |
|---|---|---|
| `waiting_for_lock` | | Another modeld process (often the daemon) holds the store lock. |
| `locked` | | The pass holds the lock. |
| `hashing` | `path`, `size` | A file is being hashed. |
| `cloning` | `path`, `size` | A duplicate is being replaced with a clone. |

A caller may stop a pass with one SIGINT only before `locked`; the pass then
ends with the error code `cancelled` and changes nothing. After `locked`, the
pass ignores a single SIGINT and runs to the end. A caller that stops reading
(or quits) does not stop the pass: write errors are ignored.

### Error codes

`store_unavailable`, `registry_unreadable`, `pass_failed`, `cancelled`,
`install_failed`, `uninstall_failed`, `unsupported`.

## Reference states

`ls` gives each app's copy of a model a `state`:

| State | Meaning |
|---|---|
| `missing` | The file is gone; the next complete pass forgets it. |
| `clone` | The file shares the stored copy's blocks. |
| `hardlinked` | The file has other hard links (or is the stored copy itself). ModelFS never replaces it. |
| `changed` | The file changed since ModelFS hashed it. |
| `copy` | An independent copy of the stored bytes. A pass can turn it into a clone. |

`space` (per model) and `totals` are in bytes:

| Field | Meaning |
|---|---|
| `on_disk` | What the copies and stored copies occupy. Clones of one stored copy count once. |
| `saved` | What sharing saves today. |
| `pending` | What a pass could still free by cloning `copy` files. |
| `store_only` | Stored copies of models that no app uses anymore. |
| `logical` | What the apps' copies would occupy without any sharing. |

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
