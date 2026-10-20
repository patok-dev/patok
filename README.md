# patok

Autonomous build loop driving agent CLIs.

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
