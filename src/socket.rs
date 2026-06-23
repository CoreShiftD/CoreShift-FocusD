// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use coreshift_core::unix_socket::UnixStreamFd;

pub enum Command {
    Status,
    Watch,
    Unknown,
}

/// Read command and peer UID from an accepted stream.
///
/// UID comes from SO_PEERCRED — not from the wire. Falls back to 0 if
/// the platform does not support SO_PEERCRED.
pub fn parse_command(stream: &UnixStreamFd) -> (Command, u32) {
    let uid = stream
        .peer_cred()
        .ok()
        .flatten()
        .map(|c| c.uid)
        .unwrap_or(0);

    let mut buf = [0u8; 64];
    let cmd = if let Ok(Some(n)) = stream.fd.read_slice(&mut buf) {
        match std::str::from_utf8(&buf[..n]).unwrap_or("").trim() {
            "status" => Command::Status,
            "watch"  => Command::Watch,
            _        => Command::Unknown,
        }
    } else {
        Command::Unknown
    };

    (cmd, uid)
}
