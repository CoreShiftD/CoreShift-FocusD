// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use coreshift_core::spawn::{SpawnOptions, SpawnBackend, Output};
use coreshift_core::CoreError;

#[derive(Debug, Clone, Default)]
pub struct Blocklist {
    pub packages: BTreeSet<String>,
}

fn run_command(cmd: &str, args: &[&str]) -> Result<Output, CoreError> {
    let mut argv = vec![cmd.to_string()];
    argv.extend(args.iter().map(|s| s.to_string()));
    SpawnOptions::builder(argv, SpawnBackend::PosixSpawn)
        .capture_stdout()
        .build()?
        .run()
}

impl Blocklist {
    pub fn load_or_create<P: AsRef<Path>>(path: P, dynamic_defaults: BTreeSet<String>, persist: bool) -> Self {
        let mut packages = BTreeSet::new();

        let static_defaults = [
            "com.google.android.as*",
            "com.google.android.gms*",
            "com.google.android.apps.wellbeing",
            "com.google.android.tts",
            "com.google.android.googlequicksearchbox",
            "com.google.android.apps.googleassistant",
            "com.google.android.permissioncontroller",
        ];

        let mut user_additions = BTreeSet::new();
        let mut user_removals = BTreeSet::new();

        if let Ok(content) = fs::read_to_string(path.as_ref()) {
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with('#') || line.is_empty() {
                    continue;
                }
                if let Some(to_remove) = line.strip_prefix('-') {
                    user_removals.insert(to_remove.trim().to_string());
                } else {
                    user_additions.insert(line.to_string());
                }
            }
        } else {
            let _ = fs::create_dir_all(path.as_ref().parent().unwrap());
        }

        // 1. Add static defaults
        for &pkg in &static_defaults {
            packages.insert(pkg.to_string());
        }

        // 2. Add dynamic defaults (if any were provided/resolved)
        for pkg in dynamic_defaults {
            packages.insert(pkg);
        }

        // 3. Add user additions
        for pkg in user_additions {
            packages.insert(pkg);
        }

        // 4. Apply user removals
        for to_remove in user_removals {
            packages.retain(|pkg| {
                if to_remove.ends_with('*') {
                    !pkg.starts_with(&to_remove[..to_remove.len() - 1])
                } else {
                    pkg != &to_remove
                }
            });
        }

        // Only write back if the file doesn't exist or if requested (e.g. dynamic defaults changed)
        // to prevent inotify infinite loops while fulfilling user's request to have the list in the file.
        if !path.as_ref().exists() || persist {
            let mut final_content = String::from("# Blocklist configuration\n# Use '-' prefix to unblock a package\n\n");
            for pkg in &packages {
                final_content.push_str(&format!("{}\n", pkg));
            }
            let _ = fs::write(path, final_content);
        }

        Self { packages }
    }

    pub fn is_blocked(&self, package: &str) -> bool {
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

    pub fn resolve_defaults() -> BTreeSet<String> {
        let mut defaults = BTreeSet::new();

        // Resolve Launcher
        if let Ok(output) = run_command("/system/bin/cmd", &["package", "resolve-activity", "-a", "android.intent.action.MAIN", "-c", "android.intent.category.HOME"]) {
            let s = String::from_utf8_lossy(&output.stdout);
            if let Some(package) = s.lines().find_map(|line| line.trim().strip_prefix("packageName=")) {
                defaults.insert(package.to_string());
            }
        }

        // Resolve Keyboard (IME)
        if let Ok(output) = run_command("/system/bin/cmd", &["settings", "get", "secure", "default_input_method"]) {
            if let Some(package) = parse_package_from_component(&output.stdout) {
                defaults.insert(package);
            }
        }

        // Resolve Accessibility Services
        if let Ok(output) = run_command("/system/bin/cmd", &["settings", "get", "secure", "enabled_accessibility_services"]) {
            let stdout = String::from_utf8_lossy(&output.stdout);
            for component in stdout.split(':') {
                if let Some(package) = parse_package_from_component_str(component) {
                    defaults.insert(package);
                }
            }
        }

        defaults
    }
}

fn parse_package_from_component(stdout: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(stdout);
    parse_package_from_component_str(s.trim())
}

fn parse_package_from_component_str(component: &str) -> Option<String> {
    let pkg = component.split('/').next()?.trim();
    if pkg.is_empty() || pkg == "null" {
        return None;
    }
    Some(pkg.to_string())
}
