// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

use std::fs;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Config {
    pub cache_dir: String,
    pub blocklist_path: String,
    pub packages_xml_path: String,
    pub socket_name: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            cache_dir: "/data/local/tmp/coreshift/".to_string(),
            blocklist_path: "/data/local/tmp/coreshift/blocklist.conf".to_string(),
            packages_xml_path: "/data/system/packages.xml".to_string(),
            socket_name: "coreshift".to_string(),
        }
    }
}

impl Config {
    pub fn load<P: AsRef<Path>>(path: P) -> Self {
        let mut config = Config::default();
        if let Ok(content) = fs::read_to_string(path) {
            for line in content.lines() {
                let line = line.trim();
                if line.starts_with('#') || line.is_empty() {
                    continue;
                }
                if let Some((key, value)) = line.split_once('=') {
                    match key.trim() {
                        "cache_dir" => config.cache_dir = value.trim().to_string(),
                        "blocklist_path" => config.blocklist_path = value.trim().to_string(),
                        "packages_xml_path" => config.packages_xml_path = value.trim().to_string(),
                        "socket_name" => config.socket_name = value.trim().to_string(),
                        _ => {}
                    }
                }
            }
        }
        config
    }
}
