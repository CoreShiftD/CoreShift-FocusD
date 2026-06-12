// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::HashMap;
use std::mem::MaybeUninit;
use coreshift_core::reactor::{Reactor, Token};
use coreshift_core::unix_socket::{bind_unix_listener, UnixSocketAddr, UnixSocketBindOptions, UnixStreamFd};
use coreshift_core::inotify::{self, read_events};
use coreshift_core::signal::{SignalRuntime, SIGTERM, SIGINT};
use coreshift_core::CoreError;
use crate::config::Config;
use crate::resolver::Resolver;
use crate::cache::UidCache;
use crate::blocklist::Blocklist;
use crate::terminal_apps::TerminalApps;
use crate::socket::{parse_command, Command};

pub struct Daemon {
    config: Config,
    resolver: Resolver,
    watchers: HashMap<Token, UnixStreamFd>,
    cgroup_monitors: HashMap<Token, (std::path::PathBuf, coreshift_core::reactor::Fd)>,
    last_v1_payload: String,
    last_max_pid: i32,
    last_package: Option<String>,
    last_broadcasted: Option<String>,
    blocklist_defaults: std::collections::BTreeSet<String>,
    terminal_apps_path: String,
}

impl Daemon {
    pub fn new(config: Config) -> Self {
        let mut cache = UidCache::new(&config.cache_dir);
        cache.load_or_refresh(&config.packages_xml_path);

        let blocklist_defaults = Blocklist::resolve_defaults();

        // Don't persist on startup unless the file is missing to avoid triggering an immediate inotify reload
        let blocklist = Blocklist::load_or_create(&config.blocklist_path, blocklist_defaults.clone(), false);

        let terminal_apps_path = format!("{}/terminal_apps.conf", config.cache_dir);
        let terminal_apps = TerminalApps::load_or_create(&terminal_apps_path);

        let resolver = Resolver::new(cache, blocklist, terminal_apps);
        Self {
            config,
            resolver,
            watchers: HashMap::new(),
            cgroup_monitors: HashMap::new(),
            last_v1_payload: String::new(),
            last_max_pid: 0,
            last_package: None,
            last_broadcasted: None,
            blocklist_defaults,
            terminal_apps_path,
        }
    }

    pub fn run(&mut self) -> Result<(), CoreError> {
        // Guard: Check if daemon is already running
        let socket_addr = UnixSocketAddr::Abstract(self.config.socket_name.as_bytes());
        if coreshift_core::unix_socket::connect_unix_stream(socket_addr).is_ok() {
            return Err(CoreError::sys(libc::EADDRINUSE, "bind"));
        }

        let mut reactor = Reactor::new()?;

        // Setup Robust Signal Handling via signalfd
        let mask = SignalRuntime::set_with(&[SIGTERM, SIGINT, libc::SIGCHLD])?;
        SignalRuntime::block_current_thread(&mask)?;
        let signal_fd = SignalRuntime::signalfd_new(&mask)?;
        let signal_token = reactor.add(&signal_fd, true, false)?;

        // Setup Socket
        let listener = bind_unix_listener(socket_addr, UnixSocketBindOptions::default())?;

        // Signal readiness to CLI
        let ready_file = format!("{}/daemon.ready", self.config.cache_dir);
        let _ = std::fs::write(ready_file, "");

        let socket_token = reactor.add(&listener.fd, true, false)?;

        // Setup Inotify using official setup helper
        let (inotify_fd, inotify_token) = reactor.setup_inotify()?;

        let wd_packages = inotify::add_watch(&inotify_fd, &self.config.packages_xml_path, inotify::MODIFY_MASK)?;
        let wd_blocklist = inotify::add_watch(&inotify_fd, &self.config.blocklist_path, inotify::MODIFY_MASK)?;
        let wd_terminal_apps = inotify::add_watch(&inotify_fd, &self.terminal_apps_path, inotify::MODIFY_MASK)?;
        let wd_foreground = inotify::add_watch(&inotify_fd, "/dev/cpuset/top-app/cgroup.procs", inotify::MODIFY_MASK)?;

        let mut events = Vec::new();
        loop {
            reactor.wait(&mut events, 64, -1)?;
            let mut refresh_needed = false;
            let mut notify_needed = false;

            for ev in &events {
                if ev.token == socket_token {
                    // Drain the edge-triggered listener
                    let mut count = 0;
                    while let Ok(Some(stream)) = listener.accept() {
                        count += 1;
                        if count > 16 { break; }
                        match parse_command(&stream) {
                            Command::Status => {
                                let res = self.resolver.resolve();
                                if let Some((pkg, _, cgroup_paths)) = res {
                                    self.update_cgroup_monitors(&mut reactor, &cgroup_paths);
                                    let response = format!("foreground: {}\ncache_entries: {}\n", pkg, self.resolver.cache.mapping.len());
                                    let _ = stream.fd.write_slice(response.as_bytes());
                                } else {
                                    let _ = stream.fd.write_slice(b"foreground: unknown\ncache_entries: 0\n");
                                }
                            }
                            Command::Watch => {
                                let res = self.resolver.resolve();
                                let current = res.as_ref().map(|r| r.0.clone());
                                self.last_package = current.clone();

                                if let Some((pkg, _, cgroup_paths)) = res {
                                    self.update_cgroup_monitors(&mut reactor, &cgroup_paths);
                                    self.last_broadcasted = Some(pkg.clone());
                                    let response = format!("{}\n", pkg);
                                    let _ = stream.fd.write_slice(response.as_bytes());
                                } else {
                                    let _ = stream.fd.write_slice(b"unknown\n");
                                }

                                if let Ok(token) = reactor.add(&stream.fd, true, false) {
                                    self.watchers.insert(token, stream);
                                }
                            }
                            Command::Unknown => {
                                let _ = stream.fd.write_slice(b"unknown command\n");
                            }
                        }
                    }
                } else if ev.token == signal_token {
                    let mut sig_info = MaybeUninit::<libc::signalfd_siginfo>::uninit();
                    let buf = unsafe {
                        std::slice::from_raw_parts_mut(
                            sig_info.as_mut_ptr() as *mut u8,
                            std::mem::size_of::<libc::signalfd_siginfo>(),
                        )
                    };
                    while let Ok(Some(_)) = signal_fd.read_slice(buf) {
                        let info = unsafe { sig_info.assume_init() };
                        if info.ssi_signo == SIGTERM as u32 || info.ssi_signo == SIGINT as u32 {
                            return Ok(());
                        }
                    }
                } else if ev.token == inotify_token {
                    let in_events = read_events(&inotify_fd)?;
                    for in_ev in in_events {
                        if in_ev.wd == wd_packages || in_ev.wd == wd_blocklist || in_ev.wd == wd_terminal_apps {
                            refresh_needed = true;
                        }
                        if in_ev.wd == wd_foreground {
                            notify_needed = true;
                        }
                    }
                } else if self.watchers.contains_key(&ev.token) {
                    if ev.error || ev.readable || ev.hangup {
                        // Client closed or sent something else, remove it
                        if let Some(stream) = self.watchers.remove(&ev.token) {
                            let _ = reactor.del(&stream.fd);
                        }
                    }
                } else if let Some((cgroup_path, _)) = self.cgroup_monitors.get(&ev.token) {
                    if ev.priority {
                        // Cgroup v2 events changed (likely populated 0)
                        let events_path = cgroup_path.join("cgroup.events");
                        if let Ok(content) = std::fs::read_to_string(events_path) {
                            if content.contains("populated 0") {
                                // Process died or moved, clear the pid cache for safety
                                self.resolver.clear_pid_cache();
                            }
                        }
                    }
                }
            }

            if refresh_needed {
                let old_fp = self.resolver.cache.fingerprint.clone();
                self.resolver.cache.load_or_refresh(&self.config.packages_xml_path);

                let mut dynamic_changed = false;
                // Only re-resolve dynamic defaults if fingerprint changed
                if self.resolver.cache.fingerprint != old_fp {
                    self.blocklist_defaults = Blocklist::resolve_defaults();
                    dynamic_changed = true;
                }

                // Only persist if dynamic defaults actually changed to avoid inotify loop on manual config edits
                self.resolver.blocklist = Blocklist::load_or_create(&self.config.blocklist_path, self.blocklist_defaults.clone(), dynamic_changed);
                self.resolver.terminal_apps = TerminalApps::load_or_create(&self.terminal_apps_path);
            }

            if notify_needed {
                // Check if CPUSet payload actually changed
                let current_v1_payload = std::fs::read_to_string("/dev/cpuset/top-app/cgroup.procs").unwrap_or_default();
                if current_v1_payload == self.last_v1_payload {
                    notify_needed = false;
                } else {
                    // Detect PID wrap-around: if max PID dropped significantly, clear cache
                    let current_max_pid = current_v1_payload.lines()
                        .filter_map(|l| l.trim().parse::<i32>().ok())
                        .max().unwrap_or(0);

                    if current_max_pid < self.last_max_pid / 2 {
                        self.resolver.clear_pid_cache();
                    }
                    self.last_max_pid = current_max_pid;
                    self.last_v1_payload = current_v1_payload;
                }
            }

            if notify_needed && !self.watchers.is_empty() {
                let res = self.resolver.resolve();
                let current_package = res.as_ref().map(|r| r.0.clone());
                self.last_package = current_package.clone();

                if let Some((pkg, _, cgroup_paths)) = res {
                    self.update_cgroup_monitors(&mut reactor, &cgroup_paths);

                    if Some(&pkg) != self.last_broadcasted.as_ref() {
                        let response = format!("{}\n", pkg);
                        let mut broken = Vec::new();
                        for (token, stream) in &self.watchers {
                            if stream.fd.write_slice(response.as_bytes()).is_err() {
                                broken.push(*token);
                            }
                        }
                        for token in broken {
                            if let Some(stream) = self.watchers.remove(&token) {
                                let _ = reactor.del(&stream.fd);
                            }
                        }
                    }
                    self.last_broadcasted = current_package;
                }
            }
        }
    }
    fn update_cgroup_monitors(&mut self, reactor: &mut Reactor, paths: &[std::path::PathBuf]) {
        // Simple logic: we only monitor the current candidates.
        // To keep it efficient, we clear old monitors that aren't in the new set.
        let new_set: std::collections::HashSet<_> = paths.iter().collect();
        let mut to_remove = Vec::new();

        for (token, (path, _)) in &self.cgroup_monitors {
            if !new_set.contains(path) {
                to_remove.push(*token);
            }
        }

        for token in to_remove {
            if let Some((_, fd)) = self.cgroup_monitors.remove(&token) {
                let _ = reactor.del(&fd);
            }
        }

        for path in paths {
            let events_path = path.join("cgroup.events");
            if !self.cgroup_monitors.values().any(|(p, _)| p == path) {
                if let Ok(file) = std::fs::File::open(&events_path) {
                    use std::os::unix::io::AsRawFd;
                    // Note: coreshift-core Fd::from_owned_raw_fd transfers ownership
                    if let Ok(fd) = unsafe { coreshift_core::reactor::Fd::from_owned_raw_fd(file.as_raw_fd(), "cgroup.events") } {
                        // Forget the file so it doesn't close yet
                        std::mem::forget(file);
                        if let Ok(token) = reactor.add_priority(&fd) {
                            self.cgroup_monitors.insert(token, (path.clone(), fd));
                        }
                    }
                }
            }
        }
    }
}
