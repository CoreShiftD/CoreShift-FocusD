// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Binder-only foreground resolution via `IActivityManager.getFocusedRootTaskInfo`.
//!
//! Uses [`coreshift_core::binder::ActivityManagerBinder`] to query the
//! ActivityManager service directly over NDK binder — no `app_process`,
//! no shell subprocess, no cgroup polling.
//!
use coreshift_core::binder::ActivityManagerBinder;
use crate::blocklist::Blocklist;

pub struct BinderForegroundSource {
    binder: ActivityManagerBinder,
}

impl BinderForegroundSource {
    pub fn try_open() -> Option<Self> {
        ActivityManagerBinder::open().ok().map(|binder| Self { binder })
    }

    pub fn resolve(&self, blocklist: &Blocklist) -> Option<String> {
        let pkg = self.binder.get_focused_package().ok()??;
        if blocklist.is_blocked(&pkg) { return None; }
        Some(pkg)
    }
}
