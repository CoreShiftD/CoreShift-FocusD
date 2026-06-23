# Configuration

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
