// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::fs;
use coreshift_core::proc::read_proc_cmdline;
use coreshift_core::uid::proc_stat;
use crate::cache::UidCache;
use crate::blocklist::Blocklist;
use crate::terminal_apps::TerminalApps;

pub struct Resolver {
    pub cache: UidCache,
    pub blocklist: Blocklist,
    pub terminal_apps: TerminalApps,
    pid_cache: std::collections::HashMap<i32, String>,
    cgroup_v2_roots: Vec<std::path::PathBuf>,
}

impl Resolver {
    pub fn new(cache: UidCache, blocklist: Blocklist, terminal_apps: TerminalApps) -> Self {
        let cgroup_v2_roots = Self::discover_cgroup_v2_roots();
        Self {
            cache,
            blocklist,
            terminal_apps,
            pid_cache: std::collections::HashMap::new(),
            cgroup_v2_roots,
        }
    }

    pub fn resolve(&mut self) -> Option<(String, bool)> {
        // Clear PID cache for each resolution cycle to handle PID recycling
        self.pid_cache.clear();

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
                if !self.blocklist.is_blocked(package) {
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
                                if cmdline.contains('/') {
                                    continue;
                                }
                            }
                        }
                        Some(pkg)
                    } else {
                        None
                    }
                } else if let Ok(cmdline) = read_proc_cmdline(pid) {
                    let pkg = cmdline.trim().to_string();
                    if !pkg.is_empty() {
                        // For system apps/processes, only allow com.android or com.google
                        if pkg.starts_with("com.android.") || pkg.starts_with("com.google.") {
                            Some(pkg)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                } else {
                    None
                };

                if let Some(pkg) = package {
                    if !self.blocklist.is_blocked(&pkg) {
                        self.pid_cache.insert(pid, pkg);
                        filtered.push(pid);
                    }
                }
            }
        }

        if filtered.is_empty() {
            return None;
        }

        // 3. Select the best remaining PID via OOM score
        self.select_best_by_oom(&filtered)
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

    fn is_pid_in_populated_v2_group(&self, pid: i32) -> bool {
        let cgroup_path = format!("/proc/{}/cgroup", pid);
        let Ok(content) = fs::read_to_string(cgroup_path) else {
            return false;
        };

        // Find the v2 path for this PID
        for line in content.lines() {
            let parts: Vec<&str> = line.split(':').collect();
            if parts.len() >= 3 && parts[0] == "0" && parts[1] == "" {
                let v2_rel_path = parts[2].trim_start_matches('/');
                for root in &self.cgroup_v2_roots {
                    let full_v2_path = root.join(v2_rel_path).join("cgroup.events");
                    if let Ok(events_content) = fs::read_to_string(full_v2_path) {
                        return events_content.contains("populated 1");
                    }
                }
            }
        }
        false
    }

    fn select_best_by_oom(&mut self, pids: &[i32]) -> Option<(String, bool)> {
        let mut best_pid = -1;
        let mut min_oom = i32::MAX;

        for &pid in pids {
            let oom_path = format!("/proc/{}/oom_score_adj", pid);
            if let Ok(oom_str) = fs::read_to_string(oom_path) {
                if let Ok(oom) = oom_str.trim().parse::<i32>() {
                    if oom < min_oom {
                        min_oom = oom;
                        best_pid = pid;
                    } else if oom == min_oom && pid > best_pid {
                        best_pid = pid;
                    }

                    // Short-circuit: 0 or less is definitive on Android
                    if min_oom <= 0 {
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
