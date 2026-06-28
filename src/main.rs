// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant};
use coreshift_foreground::blocklist::Blocklist;
use coreshift_foreground::cache::UidCache;
use coreshift_foreground::config::{Config, ResolverMode};
use coreshift_foreground::daemon::Daemon;
use coreshift_foreground::resolver::Resolver;
use coreshift_foreground::terminal_apps::TerminalApps;
use coreshift_core::unix_socket::{connect_unix_stream, connect_unix_stream_named, UnixSocketAddr, UnixConnectResult};
use coreshift_core::reactor::Reactor;
use coreshift_core::spawn::{Process, ExitStatus};
use coreshift_core::signal::{SIGTERM, SIGHUP, SIGPIPE, signal_ignore};
use coreshift_core::process::{fork, ForkResult, setsid, setpgid, redirect_stdio_to_devnull, set_pdeathsig, close_fds_from, redirect_fd_to};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let config_dir = "/data/local/tmp/coreshift";
    let _ = fs::create_dir_all(config_dir);
    let config = Config::load(&format!("{}/coreshift.conf", config_dir));

    if args.len() < 2 {
        print_usage();
        return Ok(());
    }

    match args[1].as_str() {
        "daemon" => {
            let mut config = config;
            for arg in &args[2..] {
                if let Some(val) = arg.strip_prefix("--resolver=") {
                    match ResolverMode::from_str(val) {
                        Some(m) => config.resolver_mode = m,
                        None => {
                            eprintln!("Unknown resolver '{}'. Use: auto, binder, cgroup", val);
                            return Ok(());
                        }
                    }
                }
            }
            let addr = UnixSocketAddr::Abstract(config.socket_name.as_bytes());
            if connect_unix_stream(addr).is_ok() {
                println!("Daemon is already running.");
                return Ok(());
            }
            run_supervisor(&config)?;
        }
        "status" => {
            if let Err(e) = send_command(&config.socket_name, "status") {
                eprintln!("Error: {}. Is the daemon running?", e);
            }
        }
        "watch" => {
            send_command(&config.socket_name, "watch")?;
        }
        "stop" => {
            stop_daemon(&config)?;
        }
        "restart" => {
            stop_daemon(&config)?;

            // Wait for abstract socket to be fully released
            let addr = UnixSocketAddr::Abstract(config.socket_name.as_bytes());
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(3) {
                if connect_unix_stream(addr).is_err() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
            }

            run_supervisor(&config)?;
        }
        "resolve" => {
            let debug = args.iter().any(|a| a == "--debug");
            cmd_resolve(&config, debug);
        }
        _ => {
            print_usage();
        }
    }

    Ok(())
}

fn send_command(socket_name: &str, cmd: &str) -> Result<(), Box<dyn std::error::Error>> {
    let remote = UnixSocketAddr::Abstract(socket_name.as_bytes());
    let stream = if cmd == "watch" {
        let consumer_name = format!("{socket_name}_consumer");
        match connect_unix_stream_named(remote, UnixSocketAddr::Abstract(consumer_name.as_bytes()))? {
            UnixConnectResult::Connected(s) => s,
            UnixConnectResult::InProgress(_) => return Err("Connection in progress".into()),
        }
    } else {
        match connect_unix_stream(remote)? {
            UnixConnectResult::Connected(s) => s,
            UnixConnectResult::InProgress(_) => return Err("Connection in progress".into()),
        }
    };

    let mut reactor = Reactor::new()?;
    let token = reactor.add(&stream.fd, true, false)?;

    stream.fd.write_slice(cmd.as_bytes())?;

    let mut events = Vec::new();

    loop {
        reactor.wait(&mut events, 1, -1)?;
        for ev in &events {
            if ev.token == token {
                let mut buf = [0u8; 4096];
                match stream.fd.read_slice(&mut buf)? {
                    Some(0) => return Ok(()),
                    Some(n) => {
                        io::stdout().write_all(&buf[..n])?;
                        if cmd != "watch" { return Ok(()); }
                    }
                    None => continue,
                }
            }
        }
    }
}

fn cmd_resolve(config: &Config, debug: bool) {
    let mut cache = UidCache::new(&config.cache_dir);
    cache.load_or_refresh(&config.packages_xml_path);
    let launcher  = Blocklist::resolve_launcher();
    let defaults  = Blocklist::resolve_defaults();
    let blocklist = Blocklist::load_or_create(&config.blocklist_path, defaults, false);
    let terminal  = TerminalApps::load_or_create(&format!("{}/terminal_apps.conf", config.cache_dir));
    let mut resolver = Resolver::new(cache, blocklist, terminal, launcher);

    if debug {
        let (result, trace) = resolver.resolve_traced();
        for t in &trace {
            let mark = if t.pass { "✓" } else { "✗" };
            let pid_s = if t.pid != 0 { format!("[{}] ", t.pid) } else { String::new() };
            eprintln!("{mark} {}{}: {}", pid_s, t.stage, t.detail);
        }
        match result {
            Some((pkg, is_app, _)) => println!("foreground: {pkg} (is_app={is_app})"),
            None => println!("foreground: unknown"),
        }
    } else {
        match resolver.resolve() {
            Some((pkg, _, _)) => println!("{pkg}"),
            None => println!("unknown"),
        }
    }
}

fn print_usage() {
    println!("Usage: coreshift-foreground <command> [options]");
    println!("Commands:");
    println!("  daemon [--resolver=auto|binder|cgroup]");
    println!("           Start the foreground resolution daemon (supervised)");
    println!("  stop     Stop the running daemon");
    println!("  restart  Restart the daemon");
    println!("  status   Show current foreground package");
    println!("  watch    Watch for foreground changes");
    println!("  resolve [--debug]");
    println!("           Resolve foreground package directly (no daemon). --debug traces each step");
}

fn run_supervisor(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    // Double fork to ensure complete detachment
    match unsafe { fork()? } {
        ForkResult::Parent(pid) => {
            // Parent CLI waits for middle child to ensure detachment
            let process = Process::new(pid);
            let _ = process.wait_blocking();

            // Poll for daemon readiness
            let ready_file = Path::new(&config.cache_dir).join("daemon.ready");
            let start = Instant::now();
            while !ready_file.exists() && start.elapsed() < Duration::from_secs(15) {
                std::thread::sleep(Duration::from_millis(100));
            }

            if ready_file.exists() {
                let _ = fs::remove_file(ready_file);
            } else {
                println!("Warning: Daemon start timed out (signaled ready file missing).");
            }

            return Ok(());
        }
        ForkResult::Child => {}
    }

    // Middle child process
    let _ = setsid();
    let _ = setpgid(0, 0);

    match unsafe { fork()? } {
        ForkResult::Parent(_) => {
            // Middle child exits, grandchild (supervisor) is adopted by init
            std::process::exit(0);
        }
        ForkResult::Child => {}
    }

    // Grandchild process (Supervisor)
    unsafe {
        let _ = redirect_stdio_to_devnull();
    }

    let mut crash_count = 0;
    let mut last_crash_window = Instant::now();
    let pid_file = Path::new(&config.cache_dir).join("daemon.pid");

    loop {
        match unsafe { fork()? } {
            ForkResult::Parent(daemon_pid) => {
                // Supervisor: write PID and wait
                let _ = fs::write(&pid_file, daemon_pid.to_string());

                let process = Process::new(daemon_pid);
                let status = process.wait_blocking();
                let _ = fs::remove_file(&pid_file);

                if let Ok(ExitStatus::Exited(0)) = status {
                    // Clean exit (SIGTERM handler path)
                    std::process::exit(0);
                }

                // Crash/Signal exit: handle restart with backoff
                crash_count += 1;
                if last_crash_window.elapsed() > Duration::from_secs(10) {
                    crash_count = 1;
                    last_crash_window = Instant::now();
                }

                if crash_count >= 5 {
                    eprintln!("Daemon crashed 5 times in 10s, giving up.");
                    std::process::exit(1);
                }

                let backoff = Duration::from_millis(500 * crash_count);
                std::thread::sleep(backoff);
            }
            ForkResult::Child => {
                // Daemon process (listener) — drop to daemon_uid if configured.
                let _ = set_pdeathsig(SIGTERM);
                unsafe {
                    signal_ignore(SIGHUP);
                    signal_ignore(SIGPIPE);
                }
                close_fds_from(3);

                // Redirect stderr to log file so daemon errors are visible.
                let log_path = format!("{}/daemon.log", config.cache_dir);
                if let Ok(f) = std::fs::OpenOptions::new().create(true).append(true).open(&log_path) {
                    use std::os::unix::io::IntoRawFd;
                    let fd = f.into_raw_fd();
                    unsafe { redirect_fd_to(fd, 2) };
                }

                let mut daemon = Daemon::new(config.clone());
                if let Err(_) = daemon.run() {
                    std::process::exit(1);
                }
                std::process::exit(0);
            }
        }
    }
}

fn stop_daemon(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let pid_file = Path::new(&config.cache_dir).join("daemon.pid");
    if let Ok(pid_str) = fs::read_to_string(&pid_file) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            // Send SIGTERM
            let _ = Process::new(pid).kill(SIGTERM);

            // Wait for pid file to disappear (timeout 3s)
            let start = Instant::now();
            while pid_file.exists() && start.elapsed() < Duration::from_secs(3) {
                std::thread::sleep(Duration::from_millis(100));
            }

            if pid_file.exists() {
                println!("Warning: PID file still exists after SIGTERM.");
            } else {
                println!("Daemon stopped.");
            }
        }
    } else {
        println!("Daemon not running (no PID file).");
    }
    Ok(())
}
