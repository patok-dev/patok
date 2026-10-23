# patok

Autonomous build loop driving agent CLIs.

## Interactive mode

Run a provider's own CLI in an embedded terminal, in the project directory.
Press `I` (Shift+i) or choose **Interactive** in the `m` menu (its first row, so `m` then
Enter opens the picker); the status bar has no interactive button. The picker lists the providers: ↑↓ and Enter
open one, Esc closes the picker.

| Provider  | Harness     | CLI slug |
| --------- | ----------- | -------- |
| Anthropic | Claude Code | `claude` |
| OpenAI    | Codex       | `codex`  |
| Mistral   | Vibe        | `vibe`   |

The slug is both the provider's config spelling (e.g. `builder_provider = "vibe"`) and the
program looked up on `PATH`. `mistral` is still accepted in config as an alias for `vibe`.

A provider whose program is not on `PATH` is shown disabled ("not found on
PATH"). If a build is running, the picker warns that the session shares its
files, but does not block.

Inside the session every key goes to the CLI, Esc included. `Ctrl+]` closes
the modal and ends the session (the whole process group is killed); quitting
or detaching patok does the same. When the CLI exits by itself, its final
screen stays up with a `[process exited (code N)] — Ctrl+] to close` line.

patok does not answer or pre-seed a provider's trust or first-run prompts and
never edits its config (for example `~/.claude.json`): answer them yourself in
the terminal. Session resume is not supported.

## Models

Each role (`research`, `planner`, `builder`, `reviewer`, `discovery`, `orchestrator`) has
a `<role>_provider` and a `<role>_model`. An empty model, which is the default, means the
provider's own default model.

| Harness     | Model selection | Discovery                |
| ----------- | --------------- | ------------------------ |
| Claude Code | yes (`--model`) | the `initialize` request |
| Codex       | yes (`-m`)      | `codex debug models`     |
| Vibe        | no              | none                     |

Vibe has no model flag, so patok never passes it a model. A non-empty `<role>_model` for a
Vibe role produces a config warning ("ignored: Vibe does not support model selection"),
and the settings overlay hides the model row for that role.

### Choosing a model in the settings overlay

Enter or Space on a `<role>_model` row opens a model picker for that role's provider. If
the provider was changed in the overlay and not yet saved, the picker uses the new, unsaved
value.

The entries are "Provider default" (the empty value), the models the harness reported for
the provider, the row's current value when it is not among them, and "Custom...", which
opens the usual text editor on the row. Up/Down, j/k, PageUp/PageDown, the mouse and the
wheel all move through the list. Enter selects and Esc cancels. Choosing an entry only
changes the overlay's unsaved edit. It is saved through the overlay's close dialog like any
other setting.

Any value stays possible. The list is a convenience, not a limit: a model the harness does
not report can still be typed through "Custom..." or kept as the current value. Like the
startup check, patok only warns about it and passes it to the harness unchanged.

Until a provider's models are known, a note above the list says so ("Discovering
models...", "Model discovery failed: ...", "Models not discovered yet"). The list then
offers only "Provider default", the current value and "Custom...". If the provider is being
discovered again, the note shows and the last discovered models stay listed. The list
updates while it is open when discovery finishes.

When a saved settings change, or an edited config file, assigns a role a provider that
supports model selection and has no discovered models yet, the daemon discovers that
provider's models in the background. It uses the same cache, timeout and failure rules as
at startup, and providers without model selection are never asked. Because overlay edits
apply only on save, save the provider change first and then reopen the model picker to see
the new provider's models.

### Model check at startup

When the daemon starts, it checks the configured models in the background and does not
wait for the check. Only providers that some role uses and that support model selection
are asked. They are asked in parallel, with 10 s each.

A model the harness does not report gets a warning ("... is not among the models ...
reports; it is passed ... unchanged"), and patok still passes it to the harness. The check
only warns: the harness is the authority on which models it accepts. A harness that cannot
be asked gets "<harness> models not checked: <reason>".

A trailing `[...]` suffix such as `sonnet[1m]` is ignored when comparing. Codex's hidden
models count as known. Warnings go to the daemon log: stderr, or `engine.log` next to
`engine.sock` when the daemon is detached. They can appear shortly after start. The check
uses the settings as they were at startup, so later edits do not re-run it (they can still
trigger discovery, see above). Shutdown cancels it and kills any harness it started.

### Model cache

Discovered models are cached in `$XDG_CACHE_HOME/patok/models/<slug>.json`, else
`~/.cache/patok/models/<slug>.json`. An entry is fresh for 24 h and only while the
harness `--version` matches (the first output line with a version number, so a notice printed
ahead of it does not change the key). Deleting the file forces a new discovery at the next start.
`RUST_LOG=debug patok daemon` logs each model-cache hit and miss with its reason.

Only model ids, display names and visibility are stored. The rest of Claude's response
contains account data and is never stored or logged. There is no refresh command and no
setting for this.

## Install

Supported platforms: macOS (arm64, x86_64) and Linux (x86_64).

```sh
curl -fsSL https://patok.dev/install.sh | sh
```

The script downloads the latest release archive, verifies its SHA-256 against
the release `checksums`, and installs the `patok` binary to `~/.local/bin`.
Make sure that directory is on your `PATH`.

## List running daemons

`patok ps` lists every patok engine daemon running on this machine for the
current user. It covers all projects, so `-d/--dir` has no effect.

```sh
patok ps
```

```
PROJECT               STATE    TASKS  UPTIME      PID   VERSION
/home/me/src/api      running  3/5    1 h 05 min  42    0.1.0
/home/me/src/website  idle     0/12   12 sec      1234  0.1.0
```

STATE is one of `idle`, `planning`, `running` or `discovering`. TASKS is
done/total. A `-` means the value is unknown. With no daemons it prints
`no running daemons`.

```sh
patok ps --json
```

prints a JSON array with the same fields. `uptime_secs` and `pid` are numbers,
or `null` when unknown. With no daemons it prints `[]`.

```json
[
  {
    "project": "/home/me/src/api",
    "state": "running",
    "tasks": { "done": 3, "total": 5 },
    "uptime_secs": 3900,
    "pid": 42,
    "version": "0.1.0"
  }
]
```

```sh
patok ps --watch
```

keeps running and follows daemons as they start, change and stop. On a
terminal it redraws the table in place; press Ctrl-C to quit. When stdout is
piped it prints one line per change:

```
added /home/me/src/api state=running tasks=3/5 pid=42 version=0.1.0
changed /home/me/src/api state=idle tasks=5/5 pid=42 version=0.1.0
removed /home/me/src/api
```

```sh
patok ps --watch --json
```

prints one compact JSON object per line: the same fields plus `"event"`
(`added`, `changed` or `removed`). A `removed` object carries only `event` and
`project`. It never redraws, even on a terminal.

### How daemons are found

Each daemon listens on `<runtime root>/patok/<project dir>/engine.sock`, where
the runtime root is `$XDG_RUNTIME_DIR`, else `$TMPDIR`, else `/tmp`. `ps`
scans for these sockets and lists the ones that answer. Stale sockets are
skipped and nothing is deleted.

Once listening, the daemon writes `meta.json` next to the socket (project
dir, pid, start time, engine version) and removes it on clean shutdown. `ps`
reads the pid and uptime from it, so those show as `-` if it is missing.
`--watch` notices daemons that start or stop while it runs; a crashed daemon
drops out when its connection ends.

`ps` attaches as an observer, so watching a daemon does not keep an idle
daemon alive. It still shuts down on its idle timeout as if nothing were
attached.

## Uninstall

Remove the binary and the per-user data directory:

```sh
curl -fsSL https://patok.dev/install.sh | sh -s -- --clean
```
