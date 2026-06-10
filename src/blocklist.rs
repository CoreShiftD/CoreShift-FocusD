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
    pub fn load_or_create<P: AsRef<Path>>(path: P, defaults: BTreeSet<String>) -> Self {
        let mut packages = defaults;

        if !path.as_ref().exists() {
            let _ = fs::create_dir_all(path.as_ref().parent().unwrap());
            let _ = fs::write(path.as_ref(), "# Blocklist configuration (one package per line, use - to unblock defaults)\n");
        }

        // Static defaults
        let static_defaults = [
            "com.google.android.as*",
            "com.google.android.gms*",
            "com.google.android.apps.wellbeing",
            "com.google.android.tts",
            "com.google.android.googlequicksearchbox",
            "com.google.android.apps.googleassistant",
            "com.google.android.permissioncontroller",
        ];

        for pkg in static_defaults {
            packages.insert(pkg.to_string());
        }

        if let Ok(content) = fs::read_to_string(path) {
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with('#') || line.is_empty() {
                    continue;
                }

                if let Some(to_remove) = line.strip_prefix('-') {
                    let to_remove = to_remove.trim();
                    packages.retain(|pkg| {
                        if to_remove.ends_with('*') {
                            !pkg.starts_with(&to_remove[..to_remove.len() - 1])
                        } else {
                            pkg != to_remove
                        }
                    });
                } else {
                    packages.insert(line.to_string());
                }
            }
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
        if let Ok(output) = run_command("/system/bin/cmd", &["package", "resolve-activity", "--brief", "-a", "android.intent.action.MAIN", "-c", "android.intent.category.HOME"]) {
            if let Some(package) = parse_package_from_brief(&output.stdout) {
                defaults.insert(package);
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

fn parse_package_from_brief(stdout: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(stdout);
    for word in s.split_whitespace() {
        if let Some(pkg) = word.strip_prefix("package=") {
            return Some(pkg.to_string());
        }
    }
    None
}

fn parse_package_from_component(stdout: &[u8]) -> Option<String> {
    let s = String::from_utf8_lossy(stdout);
    parse_package_from_component_str(s.trim())
}

fn parse_package_from_component_str(component: &str) -> Option<String> {
    component.split('/').next().map(|s| s.to_string())
}
