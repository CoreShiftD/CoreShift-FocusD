use std::collections::HashMap;
use coreshift_core::reactor::{Reactor, Token};
use coreshift_core::unix_socket::{bind_unix_listener, UnixSocketAddr, UnixSocketBindOptions, UnixStreamFd};
use coreshift_core::inotify;
use coreshift_core::CoreError;
use crate::config::Config;
use crate::resolver::Resolver;
use crate::cache::UidCache;
use crate::blocklist::Blocklist;
use crate::socket::{parse_command, Command};

pub struct Daemon {
    config: Config,
    resolver: Resolver,
    watchers: HashMap<Token, UnixStreamFd>,
    last_v1_payload: String,
    last_package: Option<String>,
    blocklist_defaults: std::collections::BTreeSet<String>,
}

impl Daemon {
    pub fn new(config: Config) -> Self {
        let mut cache = UidCache::new(&config.cache_dir);
        cache.load_or_refresh(&config.packages_xml_path);

        let blocklist_defaults = Blocklist::resolve_defaults();
        let blocklist = Blocklist::load(&config.blocklist_path, blocklist_defaults.clone());

        let resolver = Resolver::new(cache, blocklist);
        Self {
            config,
            resolver,
            watchers: HashMap::new(),
            last_v1_payload: String::new(),
            last_package: None,
            blocklist_defaults,
        }
    }

    pub fn run(&mut self) -> Result<(), CoreError> {
        let mut reactor = Reactor::new()?;

        // Setup Socket
        let socket_addr = UnixSocketAddr::Abstract(self.config.socket_name.as_bytes());
        let listener = bind_unix_listener(socket_addr, UnixSocketBindOptions::default())?;
        let socket_token = reactor.add(&listener.fd, true, false)?;

        // Setup Inotify
        let inotify_fd = inotify::init()?;
        let inotify_token = reactor.add(&inotify_fd, true, false)?;

        let wd_packages = inotify::add_watch(&inotify_fd, &self.config.packages_xml_path, inotify::MODIFY_MASK)?;
        let wd_blocklist = inotify::add_watch(&inotify_fd, &self.config.blocklist_path, inotify::MODIFY_MASK)?;
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
                                let foreground = self.resolver.resolve().map(|r| r.0).unwrap_or_else(|| "unknown".to_string());
                                let response = format!("{}\n", foreground);
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
                } else if ev.token == inotify_token {
                    let in_events = inotify::read_events(&inotify_fd)?;
                    for in_ev in in_events {
                        if in_ev.wd == wd_packages || in_ev.wd == wd_blocklist {
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
                self.resolver.cache.load_or_refresh(&self.config.packages_xml_path);
                self.resolver.blocklist = Blocklist::load(&self.config.blocklist_path, self.blocklist_defaults.clone());
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
                if let Some((pkg, _)) = self.resolver.resolve() {
                    if Some(&pkg) != self.last_package.as_ref() {
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
                        self.last_package = Some(pkg);
                    }
                }
            }
        }
    }
}
