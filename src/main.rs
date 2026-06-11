// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::time::{Duration, Instant};
use coreshift_foreground::config::Config;
use coreshift_foreground::blocklist::Blocklist;
use coreshift_foreground::daemon::Daemon;
use coreshift_core::unix_socket::{connect_unix_stream, UnixSocketAddr, UnixConnectResult};
use coreshift_core::reactor::Reactor;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let config = Config::load("/data/local/tmp/coreshift/coreshift.conf");

    if args.len() < 2 {
        print_usage();
        return Ok(());
    }

    match args[1].as_str() {
        "daemon" => {
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
        _ => {
            print_usage();
        }
    }

    Ok(())
}

fn send_command(socket_name: &str, cmd: &str) -> Result<(), Box<dyn std::error::Error>> {
    let addr = UnixSocketAddr::Abstract(socket_name.as_bytes());
    let stream = match connect_unix_stream(addr)? {
        UnixConnectResult::Connected(s) => s,
        UnixConnectResult::InProgress(_) => {
            return Err("Connection in progress".into());
        }
    };

    stream.fd.write_slice(cmd.as_bytes())?;

    let mut reactor = Reactor::new()?;
    let token = reactor.add(&stream.fd, true, false)?;
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

fn print_usage() {
    println!("Usage: coreshift-foreground <command>");
    println!("Commands:");
    println!("  daemon   Start the foreground resolution daemon (supervised)");
    println!("  stop     Stop the running daemon");
    println!("  restart  Restart the daemon");
    println!("  status   Show current foreground package");
    println!("  watch    Watch for foreground changes");
}

fn run_supervisor(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    // Resolve defaults once before forking to supervisor
    let blocklist_defaults = Blocklist::resolve_defaults();

    // Double fork to ensure complete detachment
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err("fork failed".into());
    }
    if pid > 0 {
        // Parent CLI waits for middle child to ensure detachment
        let mut status = 0;
        unsafe { libc::waitpid(pid, &mut status, 0); }

        // Poll for daemon readiness
        let ready_file = Path::new(&config.cache_dir).join("daemon.ready");
        let start = Instant::now();
        while !ready_file.exists() && start.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = fs::remove_file(ready_file);

        return Ok(());
    }

    // Middle child process
    unsafe {
        libc::setsid();
        libc::setpgid(0, 0);
    }

    let pid = unsafe { libc::fork() };
    if pid < 0 {
        std::process::exit(1);
    }
    if pid > 0 {
        // Middle child exits, grandchild (supervisor) is adopted by init
        std::process::exit(0);
    }

    // Grandchild process (Supervisor)
    unsafe {
        // Redirect standard I/O to /dev/null for supervisor
        if let Ok(dev_null) = fs::OpenOptions::new().read(true).write(true).open("/dev/null") {
            let fd = std::os::unix::io::AsRawFd::as_raw_fd(&dev_null);
            libc::dup2(fd, 0);
            libc::dup2(fd, 1);
            libc::dup2(fd, 2);
        }
    }

    let mut crash_count = 0;
    let mut last_crash_window = Instant::now();
    let pid_file = Path::new(&config.cache_dir).join("daemon.pid");

    loop {
        let daemon_pid = unsafe { libc::fork() };
        if daemon_pid < 0 {
            std::process::exit(1);
        }

        if daemon_pid == 0 {
            // Daemon process
            unsafe {
                // Ensure daemon dies if supervisor dies
                libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);

                // Ignore SIGPIPE to prevent death on watcher hangup
                libc::signal(libc::SIGPIPE, libc::SIG_IGN);

                // Close inherited file descriptors (except stdio)
                for i in 3..1024 {
                    libc::close(i);
                }
            }

            let mut daemon = Daemon::new_with_defaults(config.clone(), blocklist_defaults.clone());
            if let Err(_) = daemon.run() {
                std::process::exit(1);
            }
            std::process::exit(0);
        }

        // Supervisor: write PID and wait
        let _ = fs::write(&pid_file, daemon_pid.to_string());

        let mut status = 0;
        unsafe { libc::waitpid(daemon_pid, &mut status, 0); }
        let _ = fs::remove_file(&pid_file);

        if libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0 {
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
}

fn stop_daemon(config: &Config) -> Result<(), Box<dyn std::error::Error>> {
    let pid_file = Path::new(&config.cache_dir).join("daemon.pid");
    if let Ok(pid_str) = fs::read_to_string(&pid_file) {
        if let Ok(pid) = pid_str.trim().parse::<i32>() {
            // Send SIGTERM
            let _ = unsafe { libc::kill(pid, libc::SIGTERM) };

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
