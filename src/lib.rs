// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

/// Abstract Unix socket name for the FocusD foreground-change broadcast.
pub const FG_SOCKET:   &[u8] = b"coreshift";
/// Consumer socket name for the watchdog subscriber.
pub const WD_CONSUMER: &[u8] = b"coreshift_wd_consumer";
/// Consumer socket name for the preloader (pm) subscriber.
pub const PM_CONSUMER: &[u8] = b"coreshift_pm_consumer";

pub mod binder_source;
pub mod config;
pub mod blocklist;
pub mod cache;
pub mod resolver;
pub mod terminal_apps;
pub mod daemon;
pub mod socket;
