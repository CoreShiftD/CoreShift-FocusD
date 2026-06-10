# IPC Protocol

Communication between the CLI and the Daemon occurs over an **abstract Unix Domain Socket**.

- **Socket Name**: `@coreshift` (The leading `@` indicates the abstract namespace).

## Message Format

Messages are simple UTF-8 text strings.

### Commands (Client to Daemon)

| Command | Description |
| :--- | :--- |
| `status` | Request the current foreground package and cache statistics. |
| `watch` | Subscribe to a stream of foreground changes. |

### Responses (Daemon to Client)

#### `status` Response
Returns a multiline string:
```
foreground: <package_name>
cache_entries: <count>
```

#### `watch` Response
The daemon immediately sends the current foreground package name:
```
<package_name>
```
Whenever the foreground package changes, the daemon pushes a new line containing only the new package name:
```
<new_package_name>
```

## Non-Blocking I/O
The daemon implements non-blocking I/O using a single-threaded Reactor. If a client socket is not ready for writing, the daemon will drop the notification for that specific client to prevent stalling other operations.
