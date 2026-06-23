# CoreShift Foreground Resolution

Focused Android foreground package resolution service and CLI.

## Resolution Modes

### `auto` (default)
Tries binder first for push-driven watch events; falls through to cgroup for
status queries and whenever binder is unavailable.

### `binder`
Registers as `IProcessObserver` with ActivityManager. Receives
`onForegroundActivitiesChanged` callbacks over NDK binder and calls
`getFocusedRootTaskInfo` inside the callback context — the same approach as
**[sehan64](https://github.com/sehan64)**, whose binder observer technique this
mode is based on. Transaction codes are resolved at runtime by parsing
`TRANSACTION_*` static fields directly from `framework.jar` DEX — no
`app_process`, no subprocess. Status queries in binder-only mode fall through to
cgroup since direct polls outside callback context return stale data.

### `cgroup`
Reads `/dev/cpuset/top-app/cgroup.procs`, maps PIDs to packages via
`/data/system/packages.xml`, filters with OOM score and terminal-app heuristics.

## Resolver Pipeline (cgroup)

1. **Top-app CPUSet scan** — `/dev/cpuset/top-app/cgroup.procs`, descending PID order
2. **Cgroup v2 population check** — skips PIDs in unpopulated v2 groups
3. **UID → package mapping** — persistent cache keyed on `packages.xml` fingerprint
4. **Blocklist filter** — launchers, IMEs, accessibility services removed
5. **Terminal app handling** — child processes excluded; terminal packages lose OOM ties to non-terminal apps
6. **OOM score selection** — lowest `oom_score_adj` wins; ties broken as above

## Daemon & CLI

Event-driven via `inotify`, `epoll`, and (in binder/auto mode) an `eventfd`
bridging the binder thread pool to the main reactor. No polling.

```
coreshift-foreground daemon [--resolver=auto|binder|cgroup]
coreshift-foreground status
coreshift-foreground watch
coreshift-foreground stop
coreshift-foreground restart
```

Communicates over `@coreshift` abstract Unix domain socket.

## Configuration

`/data/local/tmp/coreshift/coreshift.conf` — `key = value` format.

| Key | Default | Description |
|-----|---------|-------------|
| `cache_dir` | `/data/local/tmp/coreshift/` | Runtime cache directory |
| `blocklist_path` | `<cache_dir>/blocklist.conf` | Package blocklist |
| `packages_xml_path` | `/data/system/packages.xml` | Android package registry |
| `socket_name` | `coreshift` | Abstract socket name |
| `resolver_mode` | `auto` | `auto`, `binder`, or `cgroup` |
| `daemon_uid` | *(unset)* | If set, only callers with this UID via SO_PEERCRED are served |

## Runtime Files

| Path | Description |
|------|-------------|
| `<cache_dir>/tx_code.txt` | Binder transaction code cache (`observer query api fg`) |
| `<cache_dir>/daemon.log` | Daemon stderr log |
| `<cache_dir>/daemon.pid` | PID of the supervised daemon process |
| `<cache_dir>/blocklist.conf` | Editable blocklist; inotify-reloaded live |
| `<cache_dir>/terminal_apps.conf` | Terminal emulator packages; inotify-reloaded live |

## Acknowledgements

The binder observer approach — registering as `IProcessObserver` to receive
`onForegroundActivitiesChanged` before calling `getFocusedRootTaskInfo` — is
based on the technique developed by **[sehan64](https://github.com/sehan64)**.
The DEX transaction code resolver reads the same `TRANSACTION_*` fields as
sehan64's `CodeResolver.java`, without requiring `app_process` or a subprocess.

## License

Mozilla Public License 2.0. See [LICENSE](LICENSE).
