// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::HashMap;
use std::mem::MaybeUninit;
use coreshift_core::reactor::{Reactor, Token, Fd};
use coreshift_core::unix_socket::{bind_unix_listener, UnixSocketAddr, UnixSocketBindOptions, UnixStreamFd};
use coreshift_core::inotify::{self, read_events};
use coreshift_core::signal::{SignalRuntime, SignalfdSiginfo, SIGTERM, SIGINT, SIGCHLD};
use coreshift_core::error::errno;
use coreshift_core::CoreError;
use crate::binder_source::BinderForegroundSource;
use crate::config::{Config, ResolverMode};
use crate::resolver::Resolver;
use crate::cache::UidCache;
use crate::blocklist::Blocklist;
use crate::terminal_apps::TerminalApps;
use crate::socket::{parse_command, Command};

pub struct Daemon {
    config: Config,
    binder: Option<BinderForegroundSource>,
    // Owned eventfd from IProcessObserver registration (None = no observer).
    binder_efd: Option<Fd>,
    binder_token: Option<Token>,
    resolver: Resolver,
    // Pending accepted connections waiting for a readable command.
    pending: HashMap<Token, UnixStreamFd>,
    // Active watch subscriptions keyed by caller UID.
    watchers: HashMap<u32, (Token, UnixStreamFd)>,
    // Reverse map: watcher reactor token → caller UID.
    token_to_uid: HashMap<Token, u32>,
    cgroup_monitors: HashMap<Token, (std::path::PathBuf, Fd)>,
    last_v1_payload: String,
    last_max_pid: i32,
    last_package: Option<String>,
    blocklist_defaults: std::collections::BTreeSet<String>,
    terminal_apps_path: String,
}

impl Daemon {
    pub fn new(config: Config) -> Self {
        let mut cache = UidCache::new(&config.cache_dir);
        cache.load_or_refresh(&config.packages_xml_path);

        let blocklist_defaults = Blocklist::resolve_defaults();
        let blocklist = Blocklist::load_or_create(&config.blocklist_path, blocklist_defaults.clone(), false);

        let terminal_apps_path = format!("{}/terminal_apps.conf", config.cache_dir);
        let terminal_apps = TerminalApps::load_or_create(&terminal_apps_path);

        let resolver = Resolver::new(cache, blocklist, terminal_apps);

        let (binder, binder_efd) = match BinderForegroundSource::try_open() {
            Some((src, Some(raw_efd))) => {
                let fd = unsafe { Fd::from_owned_raw_fd(raw_efd, "binder.eventfd").ok() };
                (Some(src), fd)
            }
            Some((src, None)) => (Some(src), None),
            None => (None, None),
        };

        Self {
            config,
            binder,
            binder_efd,
            binder_token: None,
            resolver,
            pending: HashMap::new(),
            watchers: HashMap::new(),
            token_to_uid: HashMap::new(),
            cgroup_monitors: HashMap::new(),
            last_v1_payload: String::new(),
            last_max_pid: 0,
            last_package: None,
            blocklist_defaults,
            terminal_apps_path,
        }
    }

    pub fn run(&mut self) -> Result<(), CoreError> {
        let socket_addr = UnixSocketAddr::Abstract(self.config.socket_name.as_bytes());
        if coreshift_core::unix_socket::connect_unix_stream(socket_addr).is_ok() {
            return Err(CoreError::sys(errno::EADDRINUSE, "bind"));
        }

        let mut reactor = Reactor::new()?;

        let mask = SignalRuntime::set_with(&[SIGTERM, SIGINT, SIGCHLD])?;
        SignalRuntime::block_current_thread(&mask)?;
        let signal_fd = SignalRuntime::signalfd_new(&mask)?;
        let signal_token = reactor.add(&signal_fd, true, false)?;

        let listener = bind_unix_listener(socket_addr, UnixSocketBindOptions::default())?;

        let ready_file = format!("{}/daemon.ready", self.config.cache_dir);
        let _ = std::fs::write(ready_file, "");

        let socket_token = reactor.add(&listener.fd, true, false)?;

        let (inotify_fd, inotify_token) = reactor.setup_inotify()?;
        let wd_packages      = inotify::add_watch(&inotify_fd, &self.config.packages_xml_path, inotify::MODIFY_MASK)?;
        let wd_blocklist     = inotify::add_watch(&inotify_fd, &self.config.blocklist_path, inotify::MODIFY_MASK)?;
        let wd_terminal_apps = inotify::add_watch(&inotify_fd, &self.terminal_apps_path, inotify::MODIFY_MASK)?;
        let wd_foreground    = inotify::add_watch(&inotify_fd, "/dev/cpuset/top-app/cgroup.procs", inotify::MODIFY_MASK)?;

        // Register binder observer eventfd if available
        if let Some(efd) = &self.binder_efd {
            if let Ok(token) = reactor.add(efd, true, false) {
                self.binder_token = Some(token);
            }
        }

        let mut events = Vec::new();
        loop {
            reactor.wait(&mut events, 64, -1)?;
            let mut refresh_needed = false;
            let mut notify_needed  = false;
            let mut binder_event   = false;

            for ev in &events {
                if ev.token == socket_token {
                    // New connections: register with epoll, read command when readable.
                    let mut count = 0;
                    while let Ok(Some(stream)) = listener.accept() {
                        count += 1;
                        if count > 16 { break; }
                        if let Ok(token) = reactor.add(&stream.fd, true, false) {
                            self.pending.insert(token, stream);
                        }
                    }
                } else if ev.token == signal_token {
                    let mut sig_info = MaybeUninit::<SignalfdSiginfo>::uninit();
                    let buf = unsafe {
                        std::slice::from_raw_parts_mut(
                            sig_info.as_mut_ptr() as *mut u8,
                            std::mem::size_of::<SignalfdSiginfo>(),
                        )
                    };
                    while let Ok(Some(_)) = signal_fd.read_slice(buf) {
                        let info = unsafe { sig_info.assume_init() };
                        if info.ssi_signo == SIGTERM as u32 || info.ssi_signo == SIGINT as u32 {
                            return Ok(());
                        }
                    }
                } else if Some(ev.token) == self.binder_token {
                    // Drain the eventfd counter — value doesn't matter
                    if let Some(efd) = &self.binder_efd {
                        let mut buf = [0u8; 8];
                        let _ = efd.read_slice(&mut buf);
                    }
                    binder_event = true;
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
                } else if self.pending.contains_key(&ev.token) {
                    // Pending connection is now readable — dispatch command.
                    if ev.error || ev.hangup {
                        self.pending.remove(&ev.token);
                        continue;
                    }
                    if ev.readable {
                        if let Some(stream) = self.pending.remove(&ev.token) {
                            let _ = reactor.del(&stream.fd);
                            let (cmd, caller_uid) = parse_command(&stream);

                            if let Some(allowed) = self.config.daemon_uid {
                                if caller_uid != allowed {
                                    continue;
                                }
                            }

                            match cmd {
                                Command::Status => {
                                    let res = self.resolve_foreground();
                                    if let Some((pkg, cgroup_paths)) = res {
                                        self.update_cgroup_monitors(&mut reactor, &cgroup_paths);
                                        let response = format!("foreground: {}\ncache_entries: {}\n", pkg, self.resolver.cache.mapping.len());
                                        let _ = stream.fd.write_slice(response.as_bytes());
                                    } else {
                                        let _ = stream.fd.write_slice(b"foreground: unknown\ncache_entries: 0\n");
                                    }
                                }
                                Command::Watch => {
                                    let res = self.resolve_foreground();
                                    self.last_package = res.as_ref().map(|r| r.0.clone());

                                    if let Some((pkg, cgroup_paths)) = res {
                                        self.update_cgroup_monitors(&mut reactor, &cgroup_paths);
                                        let _ = stream.fd.write_slice(format!("{}\n", pkg).as_bytes());
                                    } else {
                                        self.update_cgroup_monitors(&mut reactor, &[]);
                                    }

                                    if let Ok(token) = reactor.add(&stream.fd, true, false) {
                                        self.evict_watcher(&mut reactor, caller_uid);
                                        self.token_to_uid.insert(token, caller_uid);
                                        self.watchers.insert(caller_uid, (token, stream));
                                    }
                                }
                                Command::Unknown => {
                                    let _ = stream.fd.write_slice(b"unknown command\n");
                                }
                            }
                        }
                    }
                } else if let Some(&uid) = self.token_to_uid.get(&ev.token) {
                    if ev.error || ev.readable || ev.hangup {
                        self.evict_watcher(&mut reactor, uid);
                    }
                } else if let Some((cgroup_path, _)) = self.cgroup_monitors.get(&ev.token) {
                    if ev.priority {
                        let events_path = cgroup_path.join("cgroup.events");
                        if let Ok(content) = std::fs::read_to_string(events_path) {
                            if content.contains("populated 0") {
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
                if self.resolver.cache.fingerprint != old_fp {
                    self.blocklist_defaults = Blocklist::resolve_defaults();
                    dynamic_changed = true;
                }

                self.resolver.blocklist = Blocklist::load_or_create(&self.config.blocklist_path, self.blocklist_defaults.clone(), dynamic_changed);
                self.resolver.terminal_apps = TerminalApps::load_or_create(&self.terminal_apps_path);
            }

            if notify_needed {
                let current_v1_payload = std::fs::read_to_string("/dev/cpuset/top-app/cgroup.procs").unwrap_or_default();
                if current_v1_payload == self.last_v1_payload {
                    notify_needed = false;
                } else {
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

            // Binder observer event bypasses the cgroup dedup check above.
            if binder_event { notify_needed = true; }

            if notify_needed && !self.watchers.is_empty() {
                let res = self.resolve_foreground();
                let current_package = res.as_ref().map(|r| r.0.clone());

                if let Some(pkg) = &current_package {
                    if current_package != self.last_package {
                        let response = format!("{}\n", pkg);
                        let broken: Vec<u32> = self.watchers
                            .iter()
                            .filter_map(|(&uid, (_, stream))| {
                                if stream.fd.write_slice(response.as_bytes()).is_err() {
                                    Some(uid)
                                } else {
                                    None
                                }
                            })
                            .collect();
                        for uid in broken {
                            self.evict_watcher(&mut reactor, uid);
                        }
                        self.last_package = current_package;
                    }
                } else {
                    self.last_package = None;
                }

                if let Some((_, cgroup_paths)) = res {
                    self.update_cgroup_monitors(&mut reactor, &cgroup_paths);
                } else {
                    self.update_cgroup_monitors(&mut reactor, &[]);
                }
            }
        }
    }

    fn evict_watcher(&mut self, reactor: &mut Reactor, uid: u32) {
        if let Some((token, stream)) = self.watchers.remove(&uid) {
            self.token_to_uid.remove(&token);
            let _ = reactor.del(&stream.fd);
        }
    }

    fn resolve_foreground(&mut self) -> Option<(String, Vec<std::path::PathBuf>)> {
        match self.config.resolver_mode {
            ResolverMode::Cgroup => {
                self.resolver.resolve().map(|(pkg, _, paths)| (pkg, paths))
            }
            ResolverMode::Binder => {
                self.binder.as_ref()?.resolve(&self.resolver.blocklist).map(|pkg| (pkg, vec![]))
            }
            ResolverMode::Auto => {
                if let Some(binder) = &self.binder {
                    if let Some(pkg) = binder.resolve(&self.resolver.blocklist) {
                        return Some((pkg, vec![]));
                    }
                }
                self.resolver.resolve().map(|(pkg, _, paths)| (pkg, paths))
            }
        }
    }

    fn update_cgroup_monitors(&mut self, reactor: &mut Reactor, paths: &[std::path::PathBuf]) {
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
                    if let Ok(fd) = unsafe { coreshift_core::reactor::Fd::from_owned_raw_fd(file.as_raw_fd(), "cgroup.events") } {
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
