use std::env;
use std::io::{self, Write};
use coreshift_foreground::config::Config;
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
            let mut daemon = Daemon::new(config);
            daemon.run()?;
        }
        "status" => {
            send_command(&config.socket_name, "status")?;
        }
        "watch" => {
            send_command(&config.socket_name, "watch")?;
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
    println!("  daemon   Start the foreground resolution daemon");
    println!("  status   Show current foreground package");
    println!("  watch    Watch for foreground changes");
}
