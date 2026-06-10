// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::HashMap;
use coreshift_core::reactor::{Reactor, Token};
use coreshift_core::unix_socket::{bind_unix_listener, UnixSocketAddr, UnixSocketBindOptions, UnixStreamFd};
use coreshift_core::inotify;
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
    last_v1_payload: String,
    last_package: Option<String>,
    last_broadcasted: Option<String>,
    blocklist_defaults: std::collections::BTreeSet<String>,
    terminal_apps_path: String,
}

impl Daemon {
    pub fn new(config: Config) -> Self {
        let blocklist_defaults = Blocklist::resolve_defaults();
        Self::new_with_defaults(config, blocklist_defaults)
    }

    pub fn new_with_defaults(config: Config, blocklist_defaults: std::collections::BTreeSet<String>) -> Self {
        let mut cache = UidCache::new(&config.cache_dir);
        cache.load_or_refresh(&config.packages_xml_path);

        let blocklist = Blocklist::load_or_create(&config.blocklist_path, blocklist_defaults.clone(), true);

        let terminal_apps_path = format!("{}/terminal_apps.conf", config.cache_dir);
        let terminal_apps = TerminalApps::load_or_create(&terminal_apps_path);

        let resolver = Resolver::new(cache, blocklist, terminal_apps);
        Self {
            config,
            resolver,
            watchers: HashMap::new(),
            last_v1_payload: String::new(),
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

        // Setup Signal Handling
        let signal_token = reactor.setup_signalfd()?;

        // Setup Socket
        let listener = bind_unix_listener(socket_addr, UnixSocketBindOptions::default())?;

        // Signal readiness to CLI
        let ready_file = format!("{}/daemon.ready", self.config.cache_dir);
        let _ = std::fs::write(ready_file, "");

        let socket_token = reactor.add(&listener.fd, true, false)?;

        // Setup Inotify
        let inotify_fd = inotify::init()?;
        let inotify_token = reactor.add(&inotify_fd, true, false)?;

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
                    if let Ok(Some(stream)) = listener.accept() {
                        match parse_command(&stream) {
                            Command::Status => {
                                let foreground = self.resolver.resolve().map(|r| r.0).unwrap_or_else(|| "unknown".to_string());
                                let response = format!("foreground: {}\ncache_entries: {}\n", foreground, self.resolver.cache.mapping.len());
                                let _ = stream.fd.write_slice(response.as_bytes());
                            }
                            Command::Watch => {
                                let current = self.resolver.resolve().map(|r| r.0);
                                self.last_package = current.clone();
                                let display = current.as_deref().unwrap_or("unknown");
                                let response = format!("{}\n", display);
                                let _ = stream.fd.write_slice(response.as_bytes());
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
                    // SIGTERM or SIGINT received, exit cleanly
                    return Ok(());
                } else if ev.token == inotify_token {
                    let in_events = inotify::read_events(&inotify_fd)?;
                    for in_ev in in_events {
                        if in_ev.wd == wd_packages || in_ev.wd == wd_blocklist || in_ev.wd == wd_terminal_apps {
                            refresh_needed = true;
                        }
                        if in_ev.wd == wd_foreground {
                            notify_needed = true;
                        }
                    }
                } else if self.watchers.contains_key(&ev.token) {
                    if ev.error || ev.readable {
                        // Client closed or sent something else, remove it
                        if let Some(stream) = self.watchers.remove(&ev.token) {
                            let _ = reactor.del(&stream.fd);
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
                    self.last_v1_payload = current_v1_payload;
                }
            }

            if notify_needed && !self.watchers.is_empty() {
                let current_package = self.resolver.resolve().map(|r| r.0);
                self.last_package = current_package.clone();

                if current_package != self.last_broadcasted {
                    if let Some(pkg) = current_package.as_ref() {
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
}
