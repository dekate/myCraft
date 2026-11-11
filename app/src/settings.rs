//! User settings in `<root>\usr\mycraft.json`. The servers read the current
//! values on every loop, so a saved port takes effect without a restart.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::RwLock;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub web_port: u16,
    pub https_port: u16,
    pub mysql_port: u16,
    pub redis_port: u16,
    pub mailpit_port: u16,
    pub smtp_port: u16,
    /// Add a hosts entry for each new project (Quick App) and on startup.
    pub auto_vhosts: bool,
    /// Domain extension or hostname template for virtual hosts (e.g. "{name}.go", ".go", or "{name}.test").
    #[serde(alias = "hostname")]
    pub domain: String,
    pub close_to_tray: bool,
    pub db_tool: String,
    pub terminal: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            web_port: 8080,
            https_port: 443,
            mysql_port: 3306,
            redis_port: 6379,
            mailpit_port: 8025,
            smtp_port: 1025,
            auto_vhosts: true,
            domain: "{name}.go".into(),
            close_to_tray: true,
            db_tool: "HeidiSQL".into(),
            terminal: "Cmder".into(),
        }
    }
}

/// Format a project name into its virtual host domain name based on a pattern or suffix.
/// Accepts Laragon-style patterns (`{name}.test`, `{name}.go`), dotted suffixes (`.test`, `.go`),
/// bare extensions (`test`, `go`), or custom prefixes (`app.{name}.local`).
pub fn format_hostname(pattern: &str, project_name: &str) -> String {
    let p = pattern.trim().to_ascii_lowercase();
    let name = project_name.trim().to_ascii_lowercase();
    if p.is_empty() {
        return format!("{name}.go");
    }
    if p.contains("{name}") {
        p.replace("{name}", &name)
    } else if let Some(suffix) = p.strip_prefix('.') {
        format!("{name}.{suffix}")
    } else {
        format!("{name}.{p}")
    }
}

/// Extract the domain suffix (e.g. `.go`, `.test`) from a pattern or domain setting.
pub fn extract_suffix(pattern: &str) -> String {
    let p = pattern.trim().to_ascii_lowercase();
    if p.is_empty() {
        return ".go".to_string();
    }
    if let Some(pos) = p.find("{name}") {
        let after = &p[pos + "{name}".len()..];
        if !after.is_empty() {
            if after.starts_with('.') {
                after.to_string()
            } else {
                format!(".{after}")
            }
        } else {
            ".go".to_string()
        }
    } else if p.starts_with('.') {
        p.to_string()
    } else {
        format!(".{p}")
    }
}

impl Settings {
    /// Format a project folder name into its virtual host domain name.
    pub fn project_hostname(&self, project: &str) -> String {
        format_hostname(&self.domain, project)
    }

    /// The domain suffix (e.g. ".go" or ".test") for virtual host routing.
    pub fn vhost_suffix(&self) -> String {
        extract_suffix(&self.domain)
    }

    /// Extract the project folder name from a Host header or domain, if it matches this domain configuration.
    pub fn extract_project_name<'a>(&self, host: &'a str) -> Option<&'a str> {
        let suffix = self.vhost_suffix();
        host.strip_suffix(&suffix).filter(|p| !p.is_empty())
    }
}

static CURRENT: RwLock<Option<Settings>> = RwLock::new(None);

pub fn path() -> PathBuf {
    crate::mycraft_root().join("usr").join("mycraft.json")
}

/// The saved settings (defaults when there is no file yet).
pub fn current() -> Settings {
    if let Some(s) = CURRENT.read().unwrap().as_ref() {
        return s.clone();
    }
    let s: Settings = std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    *CURRENT.write().unwrap() = Some(s.clone());
    s
}

pub fn save(s: &Settings) -> Result<(), String> {
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let text = serde_json::to_string_pretty(s).map_err(|e| e.to_string())?;
    std::fs::write(&p, text).map_err(|e| format!("could not write {}: {e}", p.display()))?;
    *CURRENT.write().unwrap() = Some(s.clone());
    Ok(())
}

/// Use these settings without touching the file (tests).
#[cfg(test)]
pub fn set_for_test(s: Settings) {
    *CURRENT.write().unwrap() = Some(s);
}

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";

/// Whether Windows starts myCraft at sign-in (the per-user Run key).
pub fn autostart_enabled() -> bool {
    let mut cmd = std::process::Command::new("reg");
    cmd.args(["query", RUN_KEY, "/v", "myCraft"]);
    crate::hide_console(&mut cmd).output().map(|o| o.status.success()).unwrap_or(false)
}

pub fn set_autostart(on: bool) -> Result<(), String> {
    let mut cmd = std::process::Command::new("reg");
    if on {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        cmd.args(["add", RUN_KEY, "/v", "myCraft", "/t", "REG_SZ", "/f", "/d"]).arg(format!("\"{}\"", exe.display()));
    } else {
        cmd.args(["delete", RUN_KEY, "/v", "myCraft", "/f"]);
    }
    let out = crate::hide_console(&mut cmd).output().map_err(|e| format!("could not run reg: {e}"))?;
    if out.status.success() || (!on && !autostart_enabled()) {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_and_suffix_defaults() {
        assert_eq!(format_hostname("{name}.go", "demo"), "demo.go");
        assert_eq!(format_hostname(".go", "demo"), "demo.go");
        assert_eq!(format_hostname("go", "demo"), "demo.go");
        assert_eq!(format_hostname("", "demo"), "demo.go");
        assert_eq!(extract_suffix("{name}.go"), ".go");
        assert_eq!(extract_suffix(".go"), ".go");
        assert_eq!(extract_suffix("go"), ".go");
        assert_eq!(extract_suffix(""), ".go");
    }

    #[test]
    fn format_and_suffix_custom_domains() {
        assert_eq!(format_hostname("{name}.test", "demo"), "demo.test");
        assert_eq!(format_hostname(".test", "demo"), "demo.test");
        assert_eq!(format_hostname("test", "demo"), "demo.test");
        assert_eq!(format_hostname("{name}.local", "demo"), "demo.local");
        assert_eq!(format_hostname(".local", "demo"), "demo.local");
        assert_eq!(format_hostname("local", "demo"), "demo.local");
        assert_eq!(format_hostname("app.{name}.dev", "demo"), "app.demo.dev");

        assert_eq!(extract_suffix("{name}.test"), ".test");
        assert_eq!(extract_suffix(".test"), ".test");
        assert_eq!(extract_suffix("test"), ".test");
        assert_eq!(extract_suffix("{name}.local"), ".local");
        assert_eq!(extract_suffix(".local"), ".local");
        assert_eq!(extract_suffix("local"), ".local");
    }

    #[test]
    fn settings_methods_and_project_extraction() {
        let mut s = Settings::default();
        assert_eq!(s.project_hostname("demo"), "demo.go");
        assert_eq!(s.vhost_suffix(), ".go");
        assert_eq!(s.extract_project_name("demo.go"), Some("demo"));
        assert_eq!(s.extract_project_name("demo.test"), None);

        s.domain = "{name}.test".into();
        assert_eq!(s.project_hostname("demo"), "demo.test");
        assert_eq!(s.vhost_suffix(), ".test");
        assert_eq!(s.extract_project_name("demo.test"), Some("demo"));
        assert_eq!(s.extract_project_name("demo.go"), None);
    }

    #[test]
    fn legacy_json_deserialization_defaults_domain() {
        let legacy_json = r#"{
            "web_port": 8080,
            "mysql_port": 3306
        }"#;
        let s: Settings = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(s.domain, "{name}.go");
        assert_eq!(s.project_hostname("blog"), "blog.go");
    }

    #[test]
    fn serde_alias_hostname() {
        let json = r#"{
            "hostname": "{name}.test"
        }"#;
        let s: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(s.domain, "{name}.test");
        assert_eq!(s.project_hostname("blog"), "blog.test");
    }
}
