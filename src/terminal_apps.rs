// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

#[derive(Debug, Clone, Default)]
pub struct TerminalApps {
    pub packages: BTreeSet<String>,
}

impl TerminalApps {
    pub fn load_or_create<P: AsRef<Path>>(path: P) -> Self {
        let mut packages = BTreeSet::new();
        let defaults = [
            "com.termux",
            "com.termius.client",
            "com.server.auditor.ssh.client",
            "bin.mt*",
        ];

        if !path.as_ref().exists() {
            let mut content = String::from("# Terminal apps list (supports wildcard *)\n");
            for d in defaults {
                content.push_str(d);
                content.push('\n');
            }
            let _ = fs::create_dir_all(path.as_ref().parent().unwrap());
            let _ = fs::write(path.as_ref(), content);
        }

        if let Ok(content) = fs::read_to_string(path) {
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with('#') || line.is_empty() {
                    continue;
                }
                packages.insert(line.to_string());
            }
        }

        if packages.is_empty() {
            for d in defaults {
                packages.insert(d.to_string());
            }
        }

        Self { packages }
    }

    pub fn is_terminal(&self, package: &str) -> bool {
        for blocked in &self.packages {
            if blocked.ends_with('*') {
                if package.starts_with(&blocked[..blocked.len() - 1]) {
                    return true;
                }
            } else if blocked == package {
                return true;
            }
        }
        false
    }
}
