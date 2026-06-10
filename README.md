# CoreShift Foreground Resolution

Focused Android foreground package resolution service and CLI.

## Resolution Strategy

1.  **Top-app CPUSet Scan**: Scans `/dev/cpuset/top-app/cgroup.procs` for PIDs, sorted in descending order to prioritize recently spawned processes.
2.  **UID Mapping**: Maps PIDs to UIDs and checks against a persistent package cache.
3.  **Early Spawn Blocklist**: Filters out system defaults like Launchers, Input Methods (Keyboards), and Accessibility services.
4.  **Cgroup v2 Fallback**: If the foreground remains unresolved (especially for non-user UIDs), recursively checks Cgroup v2 `populated` groups in `/sys/fs/cgroup/apps` and `/sys/fs/cgroup`.

## UID Cache & Persistence

- **Fingerprinting**: Monitors `/data/system/packages.xml` for size and mtime changes.
- **Persistence**: Caches the UID-to-package mapping in `/data/local/tmp/coreshift/package_cache.txt`.
- **Liveness**: Refreshes the cache using `cmd package list packages -f -U` only when the fingerprint changes or a missing UID is encountered.

## Daemon & CLI

- **Event-Driven**: Fully reactive using `inotify` and a `Reactor`. No polling.
- **Socket**: Communicates over the `@coreshift` abstract Unix domain socket.
- **Commands**:
    - `daemon`: Starts the background service.
    - `status`: Returns the current foreground package.
    - `watch`: Streams foreground changes to the client (non-blocking).

## Configuration

Simple `key=value` format in `/data/local/tmp/coreshift/coreshift.conf`.

Default paths:
- `cache_dir`: `/data/local/tmp/coreshift/`
- `blocklist_path`: `/data/local/tmp/coreshift/blocklist.conf`
- `packages_xml_path`: `/data/system/packages.xml`
- `socket_name`: `coreshift`
