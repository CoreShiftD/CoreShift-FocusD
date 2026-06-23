// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Binder-only foreground resolution via `IActivityManager.getFocusedRootTaskInfo`.
//!
//! Uses [`coreshift_core::binder::ActivityManagerBinder`] to query the
//! ActivityManager service directly over NDK binder — no `app_process`,
//! no shell subprocess, no cgroup polling.
//!
//! ## Tx code resolution order
//!
//! 1. `/data/local/tmp/coreshift/tx_code.txt` (written by fgw if present).
//! 2. `ro.build.version.sdk` → known-code table.
//! 3. Linear probe over a narrow SDK-version-bound window.
//!
//! ## Usage
//!
//! ```ignore
//! if let Some(src) = BinderForegroundSource::try_open() {
//!     if let Some(pkg) = src.resolve(&blocklist) {
//!         println!("foreground: {pkg}");
//!     }
//! }
//! ```

use coreshift_core::binder::ActivityManagerBinder;
use crate::blocklist::Blocklist;

/// Foreground source backed by a direct binder call to ActivityManager.
pub struct BinderForegroundSource {
    binder: ActivityManagerBinder,
}

impl BinderForegroundSource {
    /// Open a connection to ActivityManager and resolve the transaction code.
    ///
    /// Returns `None` if libbinder_ndk is unavailable, the service is not
    /// reachable, or transaction-code resolution fails — callers should fall
    /// back to cgroup-based resolution in that case.
    pub fn try_open() -> Option<Self> {
        match ActivityManagerBinder::open() {
            Ok(binder) => Some(Self { binder }),
            Err(_) => None,
        }
    }

    /// Query ActivityManager for the currently focused app package.
    ///
    /// Returns `None` if the foreground is unknown, the transaction fails, or
    /// the resolved package is on the blocklist.
    pub fn resolve(&self, blocklist: &Blocklist) -> Option<String> {
        let pkg = self.binder.get_focused_package().ok()??;
        if blocklist.is_blocked(&pkg) {
            return None;
        }
        Some(pkg)
    }

    /// Query without applying a blocklist.
    pub fn resolve_raw(&self) -> Option<String> {
        self.binder.get_focused_package().ok()?
    }
}
