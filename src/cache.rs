use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use coreshift_core::fs::path_fingerprint;
use coreshift_core::spawn::{SpawnOptions, SpawnBackend, Output};
use coreshift_core::CoreError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Fingerprint {
    pub len: u64,
    pub modified_ns: u128,
}

impl Fingerprint {
    pub fn collect<P: AsRef<Path>>(path: P) -> Option<Self> {
        let fp = path_fingerprint(path.as_ref()).ok()?;
        Some(Self {
            len: fp.len,
            modified_ns: fp.modified_ns,
        })
    }
}

pub struct UidCache {
    pub mapping: HashMap<u32, String>,
    pub fingerprint: Option<Fingerprint>,
    cache_file: PathBuf,
    miss_counts: HashMap<u32, u8>,
}

fn run_command(cmd: &str, args: &[&str]) -> Result<Output, CoreError> {
    let mut argv = vec![cmd.to_string()];
    argv.extend(args.iter().map(|s| s.to_string()));
    SpawnOptions::builder(argv, SpawnBackend::PosixSpawn)
        .capture_stdout()
        .build()?
        .run()
}

impl UidCache {
    pub fn new(cache_dir: &str) -> Self {
        let cache_file = Path::new(cache_dir).join("package_cache.txt");
        Self {
            mapping: HashMap::new(),
            fingerprint: None,
            cache_file,
            miss_counts: HashMap::new(),
        }
    }

    pub fn load_or_refresh(&mut self, packages_xml: &str) {
        let current_fingerprint = Fingerprint::collect(packages_xml);
        let mut needs_refresh = false;

        if !self.cache_file.exists() {
            needs_refresh = true;
        } else if self.fingerprint != current_fingerprint {
            needs_refresh = true;
        }

        if needs_refresh {
            self.refresh();
            self.fingerprint = current_fingerprint;
            self.save();
        } else {
            self.load_from_file();
        }
    }

    pub fn refresh(&mut self) {
        if let Ok(output) = run_command("/system/bin/cmd", &["package", "list", "packages", "-f", "-U"]) {
            let stdout = String::from_utf8_lossy(&output.stdout);
            self.mapping = parse_package_list(&stdout);
            self.miss_counts.clear();
        }
    }

    pub fn get_package(&mut self, uid: u32) -> Option<String> {
        if let Some(pkg) = self.mapping.get(&uid) {
            return Some(pkg.clone());
        }

        // Check miss counter
        if let Some(&count) = self.miss_counts.get(&uid) {
            if count >= 3 {
                return None;
            }
        }

        // Missing UID: refresh and try again
        self.refresh();
        self.save();

        if let Some(pkg) = self.mapping.get(&uid) {
            self.miss_counts.remove(&uid);
            Some(pkg.clone())
        } else {
            let count = self.miss_counts.entry(uid).or_insert(0);
            *count += 1;
            None
        }
    }

    fn save(&self) {
        if let Some(parent) = self.cache_file.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let mut content = String::new();
        if let Some(fp) = &self.fingerprint {
            content.push_str(&format!("FINGERPRINT {} {}\n", fp.len, fp.modified_ns));
        }
        for (uid, pkg) in &self.mapping {
            content.push_str(&format!("{} {}\n", uid, pkg));
        }
        let _ = fs::write(&self.cache_file, content);
    }

    fn load_from_file(&mut self) {
        if let Ok(content) = fs::read_to_string(&self.cache_file) {
            self.mapping.clear();
            for line in content.lines() {
                let mut parts = line.split_whitespace();
                match parts.next() {
                    Some("FINGERPRINT") => {
                        if let (Some(len_str), Some(mod_str)) = (parts.next(), parts.next()) {
                            if let (Ok(len), Ok(modified_ns)) = (len_str.parse::<u64>(), mod_str.parse::<u128>()) {
                                self.fingerprint = Some(Fingerprint { len, modified_ns });
                            }
                        }
                    }
                    Some(uid_str) => {
                        if let Some(pkg) = parts.next() {
                            if let Ok(uid) = uid_str.parse::<u32>() {
                                self.mapping.insert(uid, pkg.to_string());
                            }
                        }
                    }
                    None => {}
                }
            }
        }
    }
}

pub fn parse_package_list(stdout: &str) -> HashMap<u32, String> {
    let mut mapping = HashMap::new();
    for line in stdout.lines() {
        if let Some(uid_part) = line.split("uid:").last() {
            if let Ok(uid) = uid_part.trim().parse::<u32>() {
                if let Some(pkg_part) = line.split('=').last() {
                    let pkg = pkg_part.split_whitespace().next().unwrap_or("");
                    if !pkg.is_empty() {
                        mapping.insert(uid, pkg.to_string());
                    }
                }
            }
        }
    }
    mapping
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_package_list() {
        let stdout = "package:/data/app/com.example-1/base.apk=com.example uid:10123\n                      package:/system/priv-app/SystemUI/SystemUI.apk=com.android.systemui uid:1000";
        let mapping = parse_package_list(stdout);
        assert_eq!(mapping.get(&10123).unwrap(), "com.example");
        assert_eq!(mapping.get(&1000).unwrap(), "com.android.systemui");
    }
}
