# Resolution Strategy

The foreground package is resolved using a cascaded filtering approach that minimizes overhead by narrowing candidates at each stage.

## 1. Candidate Discovery (Cgroup v1 CPUSet)
The process begins with `/dev/cpuset/top-app/cgroup.procs` (Cgroup v1).
- **Payload Comparison**: To minimize overhead, the daemon compares the current CPUSet content with the previous scan. If unchanged, resolution is skipped.
- **PID Sorting**: PIDs are sorted in **descending order** to prioritize recently spawned activity.
- **Initial Set**: These PIDs form the base set of potential foreground candidates.

## 2. Activity Filtering (Cgroup v2)
The candidate set is then filtered through the Cgroup v2 hierarchy using a highly targeted lookup.
- **Discovery**: `cgroup2` roots are discovered dynamically via `/proc/mounts`.
- **Path Lookup**: For each candidate PID, the resolver identifies its specific Cgroup v2 path by reading `/proc/<pid>/cgroup`.
- **Population check**: The candidate is only retained if its specific group's `cgroup.events` reports `populated 1`.
- **Efficiency**: This targeted approach avoids both broad tree walks and broad `/proc` scans.

## 3. Identity Resolution & Blocklist
Identity is resolved for the remaining candidates:
- **User Apps (UID >= 10000)**: Mapped via the persistent UID-to-package cache.
    - **Terminal Apps**: For known terminal apps (Termux, Termius, etc.), the resolver checks `/proc/<pid>/cmdline`. If it contains a `/`, the PID is skipped to favor the actual shell or foreground process.
- **System Processes (UID < 10000)**: Mapped via `/proc/<pid>/cmdline`. To reduce noise, **only system processes starting with `com.android.` or `com.google.` are allowed.**
- **Filtering**: Candidates are removed if their package/process name matches the blocklist (Launcher, IME, Accessibility, etc.).

## 4. Final Selection (OOM Score)
If multiple candidates survive filtering, the "best" one is chosen using OOM scores. **Broad scans of `/proc` are avoided.**
- **Scope**: Only evaluates PIDs that survived the previous v1/v2/Blocklist filters.
- **Priority**: Selects the process with the lowest `oom_score_adj` (highest kernel importance).
- **Tie-breaker**: If OOM scores are identical, the higher PID is selected.


## Blocklist
All candidates are filtered through a blocklist that includes:
- **Launcher**: Dynamically resolved via `intent.category.HOME`.
- **Input Method**: Dynamically resolved via `default_input_method` setting.
- **Accessibility**: Dynamically resolved via `enabled_accessibility_services`.
- **Custom**: User-defined packages in `blocklist.conf`.
    - Use `package.name` to block a package.
    - Use `package.prefix*` to block all packages starting with that prefix.
    - Use `-package.name` or `-package.prefix*` to **remove** a package from the blocklist (including from static defaults).
