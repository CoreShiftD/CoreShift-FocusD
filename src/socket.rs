use coreshift_core::unix_socket::UnixStreamFd;

pub enum Command {
    Status,
    Watch,
    Unknown,
}

pub fn parse_command(stream: &UnixStreamFd) -> Command {
    let mut buf = [0u8; 1024];
    if let Ok(Some(n)) = stream.fd.read_slice(&mut buf) {
        let cmd_str = String::from_utf8_lossy(&buf[..n]);
        match cmd_str.trim() {
            "status" => Command::Status,
            "watch" => Command::Watch,
            _ => Command::Unknown,
        }
    } else {
        Command::Unknown
    }
}
