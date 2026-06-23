// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use coreshift_core::binder::ActivityManagerBinder;
use crate::blocklist::Blocklist;

const TX_CACHE: &str = "/data/local/tmp/coreshift/tx_code.txt";

pub struct BinderForegroundSource {
    binder: ActivityManagerBinder,
}

impl BinderForegroundSource {
    /// Open ActivityManager binder and attempt IProcessObserver registration.
    ///
    /// Returns `(Self, Some(eventfd))` when observer registration succeeded —
    /// the eventfd becomes readable when `onForegroundActivitiesChanged` fires.
    /// Caller takes ownership of the eventfd and must close it.
    ///
    /// Returns `(Self, None)` if observer registration failed but plain binder
    /// is available (polling fallback).
    ///
    /// Returns `None` if binder is completely unavailable.
    pub fn try_open() -> Option<(Self, Option<i32>)> {
        match ActivityManagerBinder::open_with_observer(TX_CACHE) {
            Ok((binder, efd)) => Some((Self { binder }, Some(efd))),
            Err(_) => {
                ActivityManagerBinder::open(TX_CACHE).ok()
                    .map(|binder| (Self { binder }, None))
            }
        }
    }

    pub fn resolve(&self, blocklist: &Blocklist) -> Option<String> {
        let pkg = self.binder.get_focused_package().ok()??;
        if blocklist.is_blocked(&pkg) { return None; }
        Some(pkg)
    }
}
