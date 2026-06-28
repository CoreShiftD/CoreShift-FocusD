// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::fs;
use coreshift_core::proc::read_proc_cmdline;
use coreshift_core::uid::proc_stat;

pub struct TraceEntry {
    pub pid:    i32,
    pub stage:  &'static str,
    pub detail: String,
    pub pass:   bool,
}
use crate::cache::UidCache;
use crate::blocklist::Blocklist;
use crate::terminal_apps::TerminalApps;

pub struct Resolver {
    pub cache: UidCache,
    pub blocklist: Blocklist,
    pub terminal_apps: TerminalApps,
    pub launcher_pkg: Option<String>,
    pid_cache: std::collections::HashMap<i32, String>,
    cgroup_v2_roots: Vec<std::path::PathBuf>,
}

impl Resolver {
    pub fn new(cache: UidCache, blocklist: Blocklist, terminal_apps: TerminalApps, launcher_pkg: Option<String>) -> Self {
        let cgroup_v2_roots = Self::discover_cgroup_v2_roots();
        Self {
            cache,
            blocklist,
            terminal_apps,
            launcher_pkg,
            pid_cache: std::collections::HashMap::new(),
            cgroup_v2_roots,
        }
    }

    pub fn clear_pid_cache(&mut self) {
        self.pid_cache.clear();
    }

    pub fn resolve(&mut self) -> Option<(String, bool, Vec<std::path::PathBuf>)> {
        // 1. Get initial candidates from top-app CPUSet (Cgroup v1)
        let v1_pids = self.get_v1_top_app_pids("/dev/cpuset/top-app/cgroup.procs");
        if v1_pids.is_empty() {
            return None;
        }

        // 2. Resolve identities and apply initial filters (Cgroup v2 + Blocklist)
        let mut filtered = Vec::new();
        for pid in v1_pids {
            // PID-specific Cgroup v2 population check
            if !self.is_pid_in_populated_v2_group(pid) {
                continue;
            }

            if let Some(package) = self.pid_cache.get(&pid) {
                if !package.is_empty() && !self.blocklist.is_blocked(package) {
                    filtered.push(pid);
                }
                continue;
            }

            if let Ok(stat) = proc_stat(pid) {
                let package = if stat.uid >= 10000 {
                    if let Some(pkg) = self.cache.get_package(stat.uid) {
                        // Special handling for terminal apps: check cmdline for '/'
                        if self.terminal_apps.is_terminal(&pkg) {
                            if let Ok(cmdline) = read_proc_cmdline(pid) {
                                let proc_name = cmdline.split('\0').next().unwrap_or("").trim();
                                if proc_name != pkg {
                                    self.pid_cache.insert(pid, String::new());
                                    continue;
                                }
                            }
                        }
                        Some(pkg)
                    } else {
                        None
                    }
                } else if let Ok(cmdline) = read_proc_cmdline(pid) {
                    let pkg = cmdline.split('\0').next().unwrap_or("").trim();
                    if !pkg.is_empty() && (pkg.starts_with("com.android.") || pkg.starts_with("com.google.")) {
                        Some(pkg.to_string())
                    } else {
                        None
                    }
                } else {
                    None
                };

                match package {
                    Some(pkg) => {
                        self.pid_cache.insert(pid, pkg.clone());
                        if !self.blocklist.is_blocked(&pkg) {
                            filtered.push(pid);
                        }
                    }
                    None => {
                        self.pid_cache.insert(pid, String::new());
                    }
                }
            }
        }

        if filtered.is_empty() {
            return None;
        }

        // 3. Select the best remaining PID via OOM score
        let winner = self.select_best_by_oom(&filtered);

        match winner {
            Some((pkg, is_app)) => {
                let mut cgroup_paths = Vec::new();
                for pid in filtered {
                    if let Some(path) = self.get_pid_cgroup_v2_path(pid) {
                        cgroup_paths.push(path);
                    }
                }
                Some((pkg, is_app, cgroup_paths))
            }
            None => None
        }
    }

    fn get_v1_top_app_pids(&self, path: &str) -> Vec<i32> {
        let mut pids = Vec::new();
        if let Ok(content) = fs::read_to_string(path) {
            pids = content.lines()
                .filter_map(|line| line.trim().parse::<i32>().ok())
                .collect();
            pids.sort_by(|a, b| b.cmp(a)); // Descending priority
        }
        pids
    }

    fn get_pid_cgroup_v2_path(&self, pid: i32) -> Option<std::path::PathBuf> {
        let cgroup_path = format!("/proc/{}/cgroup", pid);
        let content = fs::read_to_string(cgroup_path).ok()?;

        for line in content.lines() {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 3 && parts[0] == "0" && parts[1] == "" {
                let v2_rel_path = parts[2].trim_start_matches('/');
                for root in &self.cgroup_v2_roots {
                    let full_v2_path = root.join(v2_rel_path);
                    if full_v2_path.exists() {
                        return Some(full_v2_path);
                    }
                }
            }
        }
        None
    }

    fn is_pid_in_populated_v2_group(&self, pid: i32) -> bool {
        if let Some(path) = self.get_pid_cgroup_v2_path(pid) {
            let events_path = path.join("cgroup.events");
            if let Ok(events_content) = fs::read_to_string(events_path) {
                return events_content.contains("populated 1");
            }
        }
        false
    }

    fn select_best_by_oom(&mut self, pids: &[i32]) -> Option<(String, bool)> {
        let mut best_pid = -1i32;
        let mut min_oom = i32::MAX;
        // true when current winner is deprioritized (terminal or launcher)
        let mut best_is_low_prio = true;

        for &pid in pids {
            let oom_path = format!("/proc/{}/oom_score_adj", pid);
            if let Ok(oom_str) = fs::read_to_string(oom_path) {
                if let Ok(oom) = oom_str.trim().parse::<i32>() {
                    let pkg = self.pid_cache.get(&pid).cloned().unwrap_or_default();
                    let is_low_prio = self.terminal_apps.is_terminal(&pkg)
                        || self.launcher_pkg.as_deref() == Some(pkg.as_str());

                    let beats = if oom < min_oom {
                        true
                    } else if oom == min_oom {
                        // Real app beats deprioritized (terminal/launcher) at same OOM.
                        // Among equals, higher PID wins.
                        (!is_low_prio && best_is_low_prio)
                            || (is_low_prio == best_is_low_prio && pid > best_pid)
                    } else {
                        false
                    };

                    if beats {
                        min_oom = oom;
                        best_pid = pid;
                        best_is_low_prio = is_low_prio;
                    }

                    // Short-circuit only when winner is a real (non-deprioritized) app.
                    if min_oom <= 0 && !best_is_low_prio {
                        break;
                    }
                }
            }
        }

        if best_pid != -1 {
            if let Some(package) = self.pid_cache.get(&best_pid) {
                if let Ok(stat) = proc_stat(best_pid) {
                    return Some((package.clone(), stat.uid >= 10000));
                }
            }
        }

        None
    }

    /// Like resolve() but returns a trace of every step for --debug output.
    pub fn resolve_traced(&mut self) -> (Option<(String, bool, Vec<std::path::PathBuf>)>, Vec<TraceEntry>) {
        let mut trace: Vec<TraceEntry> = Vec::new();

        let v1_path = "/dev/cpuset/top-app/cgroup.procs";
        let v1_pids = match fs::read_to_string(v1_path) {
            Ok(content) => {
                let pids: Vec<i32> = content.lines()
                    .filter_map(|l| l.trim().parse::<i32>().ok())
                    .collect();
                trace.push(TraceEntry {
                    pid: 0, stage: "cgroup-v1",
                    detail: format!("{v1_path}: {} pid(s): {:?}", pids.len(), &pids[..pids.len().min(8)]),
                    pass: !pids.is_empty(),
                });
                pids
            }
            Err(e) => {
                trace.push(TraceEntry { pid: 0, stage: "cgroup-v1",
                    detail: format!("{v1_path}: {e}"), pass: false });
                return (None, trace);
            }
        };

        let mut v2_roots_note = String::new();
        for root in &self.cgroup_v2_roots {
            v2_roots_note.push_str(&format!("{} ", root.display()));
        }
        trace.push(TraceEntry { pid: 0, stage: "cgroup-v2-roots",
            detail: v2_roots_note.trim().to_string(), pass: !self.cgroup_v2_roots.is_empty() });

        let mut filtered: Vec<i32> = Vec::new();
        for pid in &v1_pids {
            let pid = *pid;
            let cgroup_path = format!("/proc/{pid}/cgroup");
            match fs::read_to_string(&cgroup_path) {
                Err(e) => { trace.push(TraceEntry { pid, stage: "proc-cgroup",
                    detail: format!("{cgroup_path}: {e}"), pass: false }); continue; }
                Ok(content) => {
                    let v2_rel = content.lines()
                        .find(|l| l.starts_with("0::"))
                        .and_then(|l| l.splitn(3, ':').nth(2))
                        .map(|s| s.trim_start_matches('/').to_string())
                        .unwrap_or_default();
                    let mut found_root: Option<std::path::PathBuf> = None;
                    for root in &self.cgroup_v2_roots {
                        let p = root.join(&v2_rel);
                        if p.exists() { found_root = Some(p); break; }
                    }
                    match found_root {
                        None => {
                            trace.push(TraceEntry { pid, stage: "cgroup-v2-path",
                                detail: format!("no v2 path for rel={v2_rel:?}"), pass: false });
                            continue;
                        }
                        Some(ref v2path) => {
                            let events = v2path.join("cgroup.events");
                            match fs::read_to_string(&events) {
                                Err(e) => { trace.push(TraceEntry { pid, stage: "cgroup-v2-events",
                                    detail: format!("{}: {e}", events.display()), pass: false }); continue; }
                                Ok(ev) => {
                                    let populated = ev.contains("populated 1");
                                    trace.push(TraceEntry { pid, stage: "cgroup-v2-populated",
                                        detail: format!("{}: populated={populated}", v2path.display()), pass: populated });
                                    if !populated { continue; }
                                }
                            }
                        }
                    }
                }
            }

            match proc_stat(pid) {
                Err(e) => { trace.push(TraceEntry { pid, stage: "proc-stat",
                    detail: format!("{e}"), pass: false }); continue; }
                Ok(ref stat) => {
                    let pkg = if stat.uid >= 10000 {
                        self.cache.get_package(stat.uid).unwrap_or_default()
                    } else {
                        match read_proc_cmdline(pid) {
                            Ok(ref c) => c.split('\0').next().unwrap_or("").trim().to_string(),
                            Err(_) => String::new(),
                        }
                    };
                    let blocked = self.blocklist.is_blocked(&pkg);
                    trace.push(TraceEntry { pid, stage: "identity",
                        detail: format!("uid={} pkg={:?} blocked={blocked}", stat.uid, pkg),
                        pass: !blocked && !pkg.is_empty() });
                    if !blocked && !pkg.is_empty() {
                        filtered.push(pid);
                    }
                }
            }
        }

        if filtered.is_empty() {
            trace.push(TraceEntry { pid: 0, stage: "result",
                detail: "filtered empty — no foreground resolved".into(), pass: false });
            return (None, trace);
        }

        let result = self.resolve();
        trace.push(TraceEntry { pid: 0, stage: "result",
            detail: result.as_ref().map(|(p, _, _)| p.clone()).unwrap_or_else(|| "None".into()),
            pass: result.is_some() });
        (result, trace)
    }

    fn discover_cgroup_v2_roots() -> Vec<std::path::PathBuf> {
        let mut roots = Vec::new();
        if let Ok(content) = fs::read_to_string("/proc/mounts") {
            for line in content.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 3 && parts[2] == "cgroup2" {
                    roots.push(std::path::PathBuf::from(parts[1]));
                }
            }
        }
        if roots.is_empty() {
            roots.push(std::path::PathBuf::from("/sys/fs/cgroup"));
        }
        roots
    }
}
