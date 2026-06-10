# Architecture

CoreShift Foreground is a lightweight service designed to provide event-driven foreground package resolution on Android.

## Components

### 1. Daemon (`src/daemon.rs`)
The background service that monitors system triggers and maintains the foreground state.
- **Reactor**: Uses `coreshift-core`'s epoll-based reactor.
- **Inotify**: Monitors `top-app` cpuset, `packages.xml`, and the blocklist.
- **IPC**: Manages the abstract Unix socket `@coreshift`.

### 2. Resolver (`src/resolver.rs`)
The core logic for determining which package is currently in the foreground. It uses a tiered approach (CPUSet -> Cgroup v2 -> OOM).

### 3. Cache (`src/cache.rs`)
Manages the UID-to-Package mapping.
- **Persistence**: Saved to `/data/local/tmp/coreshift/package_cache.txt`.
- **Fingerprinting**: Uses `packages.xml` stats to avoid redundant refreshes.

### 4. CLI (`src/main.rs`)
A minimal binary to interact with the daemon.
- **Status**: Synchronous request/response.
- **Watch**: Non-blocking stream of foreground changes.

## Data Flow

1.  Kernel updates `/dev/cpuset/top-app/cgroup.procs`.
2.  `inotify` notifies the Daemon's Reactor.
3.  Daemon calls Resolver.
4.  Resolver scans PIDs, applies blocklist, and identifies the package.
5.  Daemon notifies all active `watch` clients via the Unix socket.
