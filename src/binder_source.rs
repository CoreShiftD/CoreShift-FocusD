// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use coreshift_core::binder::{ActivityManagerBinder, resolve_tx_codes};
use crate::blocklist::Blocklist;
use crate::resolver::TraceEntry;
use std::os::unix::io::FromRawFd;

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
        match ActivityManagerBinder::open_with_observer() {
            Ok((binder, efd)) => Some((Self { binder }, Some(efd))),
            Err(_) => ActivityManagerBinder::open().ok().map(|binder| (Self { binder }, None)),
        }
    }

    pub fn resolve(&self, blocklist: &Blocklist) -> Option<String> {
        let pkg = self.binder.get_focused_package().ok()??;
        if blocklist.is_blocked(&pkg) { return None; }
        Some(pkg)
    }

    /// Trace every step of binder resolution. Useful for `resolve --debug`.
    pub fn debug_trace(blocklist: &Blocklist) -> Vec<TraceEntry> {
        let mut t: Vec<TraceEntry> = Vec::new();

        // Step 1: tx code resolution (DEX parse of framework.jar)
        let codes = match resolve_tx_codes() {
            Err(e) => {
                t.push(TraceEntry { pid: 0, stage: "binder:tx-codes",
                    detail: e.to_string(), pass: false });
                return t;
            }
            Ok(c) => {
                t.push(TraceEntry { pid: 0, stage: "binder:tx-codes",
                    detail: format!("observer={} query={} api_mode={} fg={}",
                        c.observer_code, c.query_code, c.api_mode, c.fg_code),
                    pass: true });
                c
            }
        };
        let _ = codes;

        // Step 2: open with observer (requires SET_ACTIVITY_WATCHER — fails for shell)
        match ActivityManagerBinder::open_with_observer() {
            Ok((binder, efd)) => {
                // close the eventfd we won't be using
                unsafe { std::fs::File::from_raw_fd(efd) };
                t.push(TraceEntry { pid: 0, stage: "binder:open-with-observer",
                    detail: "ok — observer registered".into(), pass: true });
                let src = Self { binder };
                match src.binder.get_focused_package() {
                    Ok(Some(pkg)) => {
                        let blocked = blocklist.is_blocked(&pkg);
                        t.push(TraceEntry { pid: 0, stage: "binder:get-focused",
                            detail: format!("{pkg} (blocked={blocked})"), pass: !blocked });
                    }
                    Ok(None) => { t.push(TraceEntry { pid: 0, stage: "binder:get-focused",
                        detail: "returned None (no focused task)".into(), pass: false }); }
                    Err(e) => { t.push(TraceEntry { pid: 0, stage: "binder:get-focused",
                        detail: e.to_string(), pass: false }); }
                }
            }
            Err(e) => {
                t.push(TraceEntry { pid: 0, stage: "binder:open-with-observer",
                    detail: e.to_string(), pass: false });

                // Step 3: fall back to plain open (no observer)
                match ActivityManagerBinder::open() {
                    Err(e2) => {
                        t.push(TraceEntry { pid: 0, stage: "binder:open",
                            detail: e2.to_string(), pass: false });
                    }
                    Ok(binder) => {
                        t.push(TraceEntry { pid: 0, stage: "binder:open",
                            detail: "ok (polling mode)".into(), pass: true });
                        let src = Self { binder };
                        match src.binder.get_focused_package() {
                            Ok(Some(pkg)) => {
                                let blocked = blocklist.is_blocked(&pkg);
                                t.push(TraceEntry { pid: 0, stage: "binder:get-focused",
                                    detail: format!("{pkg} (blocked={blocked})"), pass: !blocked });
                            }
                            Ok(None) => { t.push(TraceEntry { pid: 0, stage: "binder:get-focused",
                                detail: "returned None (no focused task)".into(), pass: false }); }
                            Err(e2) => { t.push(TraceEntry { pid: 0, stage: "binder:get-focused",
                                detail: e2.to_string(), pass: false }); }
                        }
                    }
                }
            }
        }
        t
    }
}
