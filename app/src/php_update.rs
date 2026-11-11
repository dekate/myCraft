//! PHP security updates. Each installed PHP line (8.2, 8.3, …) under
//! `bin\php` is compared with windows.php.net's `releases.json`; an outdated
//! line gets its newest patch installed next to the old folder (php.ini and
//! extra extension DLLs carried over). Projects resolve to the newest build of
//! the line they need (`for_project`), so they move to the patched PHP on the
//! next request.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const RELEASES: &str = "https://windows.php.net/downloads/releases/";

/// A PHP build folder, e.g. `php-8.2.35-1-nts-Win32-vs16-x64`.
#[derive(Clone, Debug, PartialEq)]
pub struct Build {
    pub dir_name: String,
    /// `[major, minor, patch]`
    pub version: Vec<u32>,
    /// Windows rebuild number (`8.2.35-1` → 1), 0 when absent.
    pub rebuild: u32,
    pub nts: bool,
    pub vc: String,
    pub arch: String,
}

impl Build {
    pub fn parse(dir_name: &str) -> Option<Build> {
        let parts: Vec<String> = dir_name
            .strip_prefix("php-")?
            .split('-')
            .map(|p| p.to_ascii_lowercase())
            .collect();
        let (version, rebuild) = parse_version(&parts[..parts.len().min(2)].join("-"))?;
        let vc = parts.iter().find(|p| p.starts_with("vc") || p.starts_with("vs"))?.clone();
        let arch = parts.last()?.clone();
        if arch != "x64" && arch != "x86" {
            return None;
        }
        Some(Build {
            dir_name: dir_name.to_string(),
            version,
            rebuild,
            nts: parts.iter().any(|p| p == "nts"),
            vc,
            arch,
        })
    }

    /// `"8.2"`, the key `releases.json` uses.
    pub fn line(&self) -> String {
        format!("{}.{}", self.version[0], self.version[1])
    }

    pub fn display(&self) -> String {
        let v: Vec<String> = self.version.iter().map(u32::to_string).collect();
        format!("{}{}", v.join("."), if self.nts { " NTS" } else { "" })
    }

    fn order(&self) -> (Vec<u32>, u32) {
        (self.version.clone(), self.rebuild)
    }
}

/// `"8.2.35-1"` → `([8, 2, 35], 1)`; `"8.2.27"` → `([8, 2, 27], 0)`.
fn parse_version(s: &str) -> Option<(Vec<u32>, u32)> {
    let mut it = s.splitn(2, '-');
    let version: Vec<u32> = it
        .next()?
        .split('.')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    if version.len() != 3 {
        return None;
    }
    let rebuild = it.next().and_then(|r| r.parse().ok()).unwrap_or(0);
    Some((version, rebuild))
}

/// Installed builds that have a `php-cgi.exe`.
pub fn installed(bin_dir: &Path) -> Vec<Build> {
    let Ok(entries) = fs::read_dir(bin_dir) else { return Vec::new() };
    entries
        .flatten()
        .filter(|e| e.path().join("php-cgi.exe").is_file())
        .filter_map(|e| Build::parse(&e.file_name().to_string_lossy()))
        .collect()
}

/// The newest installed build: the one sites are served with.
pub fn newest(bin_dir: &Path) -> Option<PathBuf> {
    installed(bin_dir)
        .into_iter()
        .max_by_key(Build::order)
        .map(|b| bin_dir.join(b.dir_name))
}

/// Per-project pin, like phpenv/asdf: `8.2` (or `8.2.35`) in `<project>\.php-version`.
const PIN_FILE: &str = ".php-version";

pub fn pinned(project_dir: &Path) -> Option<String> {
    let pin = fs::read_to_string(project_dir.join(PIN_FILE)).ok()?;
    let pin = pin.trim();
    (!pin.is_empty()).then(|| pin.to_string())
}

/// Pin `line` (e.g. "8.2") for the project, or `None` to go back to automatic.
pub fn set_pin(project_dir: &Path, line: Option<&str>) -> std::io::Result<()> {
    let file = project_dir.join(PIN_FILE);
    match line {
        Some(l) => fs::write(file, format!("{l}\n")),
        None if file.exists() => fs::remove_file(file),
        None => Ok(()),
    }
}

/// `require.php` from the project's composer.json, e.g. `^8.2`.
pub fn composer_php(project_dir: &Path) -> Option<String> {
    let text = fs::read_to_string(project_dir.join("composer.json")).ok()?;
    let json: Value = serde_json::from_str(&text).ok()?;
    Some(json.get("require")?.get("php")?.as_str()?.to_string())
}

/// The PHP build a project runs on: its `.php-version` pin, else the newest
/// build satisfying composer.json's `require.php`, else the newest build.
/// Errs when the project asks for a PHP that isn't installed, rather than
/// running it on one it can't parse.
pub fn for_project(bin_dir: &Path, project_dir: &Path) -> Result<Build, String> {
    let mut builds = installed(bin_dir);
    builds.sort_by_key(|b| std::cmp::Reverse(b.order()));
    let have = || {
        let v: Vec<String> = builds.iter().map(Build::display).collect();
        if v.is_empty() { "none".to_string() } else { v.join(", ") }
    };
    if let Some(pin) = pinned(project_dir) {
        let want: Vec<u32> = pin.split('.').filter_map(|p| p.parse().ok()).collect();
        return builds
            .iter()
            .find(|b| !want.is_empty() && b.version.starts_with(&want))
            .cloned()
            .ok_or_else(|| format!("This project is pinned to PHP {pin} (.php-version), which is not installed. Installed: {}.", have()));
    }
    if let Some(req) = composer_php(project_dir) {
        return builds
            .iter()
            .find(|b| satisfies(&req, &b.version))
            .cloned()
            .ok_or_else(|| format!("composer.json requires PHP {req}, which is not installed. Installed: {}.", have()));
    }
    builds.first().cloned().ok_or_else(|| "No PHP found in bin\\php.".to_string())
}

/// Composer version constraint check for the forms projects use for PHP:
/// `^8.2`, `~8.1.0`, `>=7.4 <8.4`, `8.2.*`, `^7.4|^8.0`, `>=8.1,<8.4`.
/// Unrecognised terms don't restrict.
pub fn satisfies(constraint: &str, v: &[u32]) -> bool {
    constraint.split('|').filter(|a| !a.trim().is_empty()).any(|alt| {
        alt.split(|c: char| c == ',' || c.is_whitespace())
            .filter(|t| !t.is_empty())
            .all(|t| term_ok(t, v))
    })
}

fn term_ok(term: &str, v: &[u32]) -> bool {
    let term = term.split('@').next().unwrap_or(term);
    let (op, rest) = ["^", "~", ">=", "<=", "!=", "==", ">", "<", "="]
        .iter()
        .find_map(|op| term.strip_prefix(op).map(|r| (*op, r)))
        .unwrap_or(("", term));
    let rest = rest.trim_start_matches('v');
    let nums: Vec<u32> = rest.split('.').map_while(|p| p.parse().ok()).collect();
    if nums.is_empty() {
        return true;
    }
    let n = nums.len();
    let mut lo = nums.clone();
    lo.resize(3, 0);
    let bump = |i: usize| {
        let mut hi = lo.clone();
        hi[i] += 1;
        hi[i + 1..].iter_mut().for_each(|x| *x = 0);
        hi
    };
    match op {
        "^" => v >= &lo[..] && v < &bump(nums.iter().position(|&x| x != 0).unwrap_or(n - 1))[..],
        "~" => v >= &lo[..] && v < &bump(n.saturating_sub(2))[..],
        ">=" => v >= &lo[..],
        ">" => v > &lo[..],
        "<=" => v <= &lo[..],
        "<" => v < &lo[..],
        "!=" => v != &lo[..],
        // `8.2`, `8.2.*`, `=8.2.27`: everything given must match.
        _ => v.starts_with(&nums),
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub zip: String,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Current,
    Outdated(Release),
    /// The line is gone from releases.json: it gets no more fixes.
    Unsupported,
}

#[derive(Clone, Debug)]
pub struct Report {
    pub build: Build,
    pub status: Status,
}

/// The latest release of `b`'s line in the same flavour (TS/NTS, arch).
fn find_release(json: &Value, b: &Build) -> Option<Release> {
    let line = json.get(b.line())?;
    let version = line.get("version")?.as_str()?;
    let flavour = if b.nts { "nts-" } else { "ts-" };
    let arch = format!("-{}", b.arch);
    let exact = format!("{flavour}{}{arch}", b.vc);
    // Prefer the same compiler; a line can move to a newer one between patches.
    let build = line.get(&exact).or_else(|| {
        line.as_object()?
            .iter()
            .find(|(k, _)| k.starts_with(flavour) && k.ends_with(&arch))
            .map(|(_, v)| v)
    })?;
    let zip = build.get("zip")?;
    Some(Release {
        version: version.to_string(),
        zip: zip.get("path")?.as_str()?.to_string(),
        sha256: zip.get("sha256")?.as_str()?.to_ascii_lowercase(),
    })
}

/// One report per (line, TS/NTS, arch), for the newest installed build of each.
pub fn reports(json: &Value, builds: &[Build]) -> Vec<Report> {
    let mut newest: Vec<Build> = Vec::new();
    for b in builds {
        match newest
            .iter_mut()
            .find(|n| n.line() == b.line() && n.nts == b.nts && n.arch == b.arch)
        {
            Some(n) if b.order() > n.order() => *n = b.clone(),
            Some(_) => {}
            None => newest.push(b.clone()),
        }
    }
    newest.sort_by_key(|b| std::cmp::Reverse(b.order()));
    newest
        .into_iter()
        .map(|build| {
            let status = match find_release(json, &build) {
                None => Status::Unsupported,
                Some(r) => match parse_version(&r.version) {
                    Some(v) if v > build.order() => Status::Outdated(r),
                    _ => Status::Current,
                },
            };
            Report { build, status }
        })
        .collect()
}

fn fetch_releases() -> Result<Value, String> {
    crate::packages::agent()
        .get(&format!("{RELEASES}releases.json"))
        .call()
        .map_err(|e| format!("could not reach windows.php.net: {e}"))?
        .into_json()
        .map_err(|e| format!("bad releases.json: {e}"))
}

/// Download, verify and unpack `rel` beside `from`. Returns the new folder.
fn install(bin_dir: &Path, from: &Build, rel: &Release) -> Result<PathBuf, String> {
    if !rel.zip.starts_with("php-") || rel.sha256.len() != 64 {
        return Err(format!("unexpected release entry {}", rel.zip));
    }
    let name = rel.zip.trim_end_matches(".zip");
    let fresh = !bin_dir.join(name).exists();
    let d = crate::packages::Download {
        url: format!("{RELEASES}{}", rel.zip),
        file_name: rel.zip.clone(),
        sha256: Some(rel.sha256.clone()),
    };
    let dest = crate::packages::install(&d, bin_dir)?;
    if !dest.join("php-cgi.exe").is_file() {
        return Err(format!("{} has no php-cgi.exe", dest.display()));
    }
    if fresh {
        carry_over(&bin_dir.join(&from.dir_name), &dest, &from.dir_name, name);
    }
    Ok(dest)
}

/// Keep the user's php.ini (paths pointing at the old folder rewritten) and any
/// extension DLLs they added, e.g. xdebug or redis. Same line, same ABI.
/// Extensions a fresh php.ini turns on: what Laravel, WordPress and Composer need.
const DEFAULT_EXTENSIONS: &[&str] = &["curl", "fileinfo", "gd", "intl", "mbstring", "mysqli", "openssl", "pdo_mysql", "pdo_sqlite", "sqlite3", "zip"];

/// Make sure a PHP build has a php.ini: copy php.ini-development, point
/// extension_dir at ext, enable the common extensions that ship with the
/// build, and set a timezone. Returns the path and whether it was created.
pub fn ensure_ini(build_dir: &Path) -> Result<(PathBuf, bool), String> {
    let ini = build_dir.join("php.ini");
    if ini.is_file() {
        return Ok((ini, false));
    }
    let template = fs::read_to_string(build_dir.join("php.ini-development"))
        .map_err(|e| format!("{} has no php.ini-development: {e}", build_dir.display()))?;
    let has_ext = |name: &str| build_dir.join("ext").join(format!("php_{name}.dll")).is_file();
    let mut out = String::with_capacity(template.len());
    for line in template.lines() {
        let t = line.trim();
        let enabled = t
            .strip_prefix(";extension=")
            .map(|e| e.trim().trim_start_matches("php_").trim_end_matches(".dll"))
            .filter(|e| DEFAULT_EXTENSIONS.contains(e) && has_ext(e));
        if let Some(e) = enabled {
            out.push_str(&format!("extension={e}"));
        } else if t.starts_with(";extension_dir") && t.contains("\"ext\"") {
            out.push_str("extension_dir = \"ext\"");
        } else if t.starts_with(";date.timezone") {
            out.push_str("date.timezone = UTC");
        } else {
            out.push_str(line);
        }
        out.push_str("\r\n");
    }
    fs::write(&ini, out).map_err(|e| format!("could not write {}: {e}", ini.display()))?;
    Ok((ini, true))
}

/// Render phpinfo() of one build to `<root>\tmp\phpinfo-<build>.html` with its php-cgi.
pub fn phpinfo_html(build_dir: &Path) -> Result<PathBuf, String> {
    let tmp = crate::mycraft_root().join("tmp");
    fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let script = tmp.join("phpinfo.php");
    fs::write(&script, "<?php phpinfo();").map_err(|e| e.to_string())?;
    let mut cmd = std::process::Command::new(build_dir.join("php-cgi.exe"));
    cmd.arg("-q").arg(&script).current_dir(build_dir).stdin(std::process::Stdio::null());
    let out = crate::hide_console(&mut cmd).output().map_err(|e| format!("could not run php-cgi: {e}"))?;
    if out.stdout.is_empty() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_string());
    }
    let name = build_dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let html = tmp.join(format!("phpinfo-{name}.html"));
    fs::write(&html, &out.stdout).map_err(|e| e.to_string())?;
    Ok(html)
}

fn carry_over(old: &Path, new: &Path, old_name: &str, new_name: &str) {
    if let Ok(ini) = fs::read_to_string(old.join("php.ini")) {
        let _ = fs::write(new.join("php.ini"), ini.replace(old_name, new_name));
    }
    let Ok(entries) = fs::read_dir(old.join("ext")) else { return };
    for e in entries.flatten() {
        let target = new.join("ext").join(e.file_name());
        if !target.exists() {
            let _ = fs::copy(e.path(), target);
        }
    }
}

#[derive(Default)]
struct Shared {
    busy: bool,
    message: String,
    reports: Vec<Report>,
}

/// Background check/update, polled by the UI each frame.
#[derive(Clone, Default)]
pub struct PhpUpdater {
    shared: Arc<Mutex<Shared>>,
}

impl PhpUpdater {
    pub fn snapshot(&self) -> (bool, String, Vec<Report>) {
        let s = self.shared.lock().unwrap();
        (s.busy, s.message.clone(), s.reports.clone())
    }

    pub fn check(&self, bin_dir: PathBuf) {
        self.run(bin_dir, false);
    }

    /// Install the newest patch of every outdated line, then re-check.
    pub fn update_all(&self, bin_dir: PathBuf) {
        self.run(bin_dir, true);
    }

    fn run(&self, bin_dir: PathBuf, update: bool) {
        {
            let mut s = self.shared.lock().unwrap();
            if s.busy {
                return;
            }
            s.busy = true;
            s.message = if update { "Downloading PHP updates…" } else { "Checking windows.php.net…" }.into();
        }
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            let outcome = (|| {
                let json = fetch_releases()?;
                let mut done = Vec::new();
                if update {
                    for r in reports(&json, &installed(&bin_dir)) {
                        if let Status::Outdated(rel) = &r.status {
                            shared.lock().unwrap().message = format!("Installing PHP {}…", rel.version);
                            install(&bin_dir, &r.build, rel)?;
                            done.push(rel.version.clone());
                        }
                    }
                }
                let reports = reports(&json, &installed(&bin_dir));
                let outdated = reports.iter().filter(|r| matches!(r.status, Status::Outdated(_))).count();
                let msg = if !done.is_empty() {
                    format!("Installed PHP {}. Old folders are kept; delete them when you no longer need them.", done.join(", "))
                } else if outdated > 0 {
                    format!("{outdated} PHP line(s) have security updates.")
                } else {
                    "All installed PHP lines are on their latest patch.".to_string()
                };
                Ok::<_, String>((reports, msg))
            })();
            let mut s = shared.lock().unwrap();
            match outcome {
                Ok((reports, msg)) => {
                    s.reports = reports;
                    s.message = msg;
                }
                Err(e) => s.message = format!("PHP update failed: {e}"),
            }
            s.busy = false;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_build_dirs() {
        let b = Build::parse("php-8.2.27-Win32-vs16-x64").unwrap();
        assert_eq!((b.version.clone(), b.rebuild, b.nts), (vec![8, 2, 27], 0, false));
        assert_eq!((b.vc.as_str(), b.arch.as_str(), b.line()), ("vs16", "x64", "8.2".into()));

        let b = Build::parse("php-8.2.35-1-nts-Win32-vs16-x64").unwrap();
        assert_eq!((b.version.clone(), b.rebuild, b.nts), (vec![8, 2, 35], 1, true));

        let b = Build::parse("php-5.4.9-nts-Win32-VC9-x86").unwrap();
        assert_eq!((b.vc.as_str(), b.arch.as_str()), ("vc9", "x86"));

        assert!(Build::parse("php-latest").is_none());
        assert!(Build::parse("adminer").is_none());
    }

    fn releases() -> Value {
        json!({
            "8.2": {
                "version": "8.2.35-1",
                "ts-vs16-x64": {"zip": {"path": "php-8.2.35-1-Win32-vs16-x64.zip", "sha256": "AB"}},
                "nts-vs16-x64": {"zip": {"path": "php-8.2.35-1-nts-Win32-vs16-x64.zip", "sha256": "cd"}}
            },
            "8.4": {
                "version": "8.4.27-1",
                "ts-vs17-x64": {"zip": {"path": "php-8.4.27-1-Win32-vs17-x64.zip", "sha256": "ef"}}
            }
        })
    }

    #[test]
    fn reports_newest_build_per_line() {
        let builds: Vec<Build> = [
            "php-5.4.9-nts-Win32-VC9-x86",
            "php-8.2.27-Win32-vs16-x64",
            "php-8.2.35-1-Win32-vs16-x64",
            "php-8.2.20-nts-Win32-vs16-x64",
            "php-8.4.1-Win32-vs16-x64",
        ]
        .iter()
        .filter_map(|n| Build::parse(n))
        .collect();
        let r = reports(&releases(), &builds);
        let by = |dir: &str| r.iter().find(|r| r.build.dir_name == dir).map(|r| r.status.clone());

        assert_eq!(r.len(), 4, "8.2.27 is superseded by the installed 8.2.35-1");
        assert_eq!(by("php-8.2.35-1-Win32-vs16-x64"), Some(Status::Current));
        assert_eq!(by("php-5.4.9-nts-Win32-VC9-x86"), Some(Status::Unsupported));
        match by("php-8.2.20-nts-Win32-vs16-x64") {
            Some(Status::Outdated(rel)) => assert_eq!(rel.zip, "php-8.2.35-1-nts-Win32-vs16-x64.zip"),
            other => panic!("{other:?}"),
        }
        // 8.4 moved from vs16 to vs17: fall back to the same flavour and arch.
        match by("php-8.4.1-Win32-vs16-x64") {
            Some(Status::Outdated(rel)) => assert_eq!((rel.zip.as_str(), rel.sha256.as_str()), ("php-8.4.27-1-Win32-vs17-x64.zip", "ef")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn composer_constraints() {
        let ok = |c: &str, v: [u32; 3]| satisfies(c, &v);
        assert!(ok("^8.2", [8, 2, 35]) && ok("^8.2", [8, 4, 1]) && !ok("^8.2", [8, 1, 34]) && !ok("^8.2", [9, 0, 0]));
        assert!(ok("~8.1.0", [8, 1, 34]) && !ok("~8.1.0", [8, 2, 0]));
        assert!(ok("~8.1", [8, 4, 0]) && !ok("~8.1", [9, 0, 0]));
        assert!(ok(">=7.4 <8.4", [8, 3, 1]) && !ok(">=7.4 <8.4", [8, 4, 0]));
        assert!(ok(">=8.1,<8.4", [8, 1, 0]) && !ok(">=8.1,<8.4", [7, 4, 33]));
        assert!(ok("^7.4|^8.0", [7, 4, 33]) && ok("^7.4 || ^8.0", [8, 2, 0]) && !ok("^7.4|^8.0", [7, 1, 33]));
        assert!(ok("8.2.*", [8, 2, 27]) && !ok("8.2.*", [8, 3, 0]));
        assert!(ok(">=8.2@dev", [8, 2, 0]) && ok("*", [5, 4, 9]));
    }

    #[test]
    fn project_resolution() {
        let root = std::env::temp_dir().join(format!("mycraft-php-test-{}", std::process::id()));
        let bin = root.join("bin");
        let proj = root.join("proj");
        for d in ["php-5.4.9-nts-Win32-VC9-x86", "php-8.2.35-1-Win32-vs16-x64", "php-8.4.27-1-Win32-vs17-x64"] {
            fs::create_dir_all(bin.join(d)).unwrap();
            fs::write(bin.join(d).join("php-cgi.exe"), "").unwrap();
        }
        fs::create_dir_all(&proj).unwrap();
        let pick = || for_project(&bin, &proj).map(|b| b.dir_name);

        assert_eq!(pick().unwrap(), "php-8.4.27-1-Win32-vs17-x64", "newest by default");
        fs::write(proj.join("composer.json"), r#"{"require":{"php":"~8.2.0"}}"#).unwrap();
        assert_eq!(pick().unwrap(), "php-8.2.35-1-Win32-vs16-x64", "composer constraint");
        fs::write(proj.join("composer.json"), r#"{"require":{"php":"^7.4"}}"#).unwrap();
        assert!(pick().unwrap_err().contains("requires PHP ^7.4"));
        set_pin(&proj, Some("5.4")).unwrap();
        assert_eq!(pick().unwrap(), "php-5.4.9-nts-Win32-VC9-x86", "pin wins over composer");
        set_pin(&proj, Some("8.3")).unwrap();
        assert!(pick().unwrap_err().contains("pinned to PHP 8.3"));
        set_pin(&proj, None).unwrap();
        assert!(pinned(&proj).is_none());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn rejects_unsafe_zip_names() {
        let from = Build::parse("php-8.2.27-Win32-vs16-x64").unwrap();
        let bad = Release { version: "8.2.35".into(), zip: "..\\..\\evil.zip".into(), sha256: "0".repeat(64) };
        assert!(install(Path::new("."), &from, &bad).is_err());
    }

    #[test]
    fn fresh_ini_enables_shipped_extensions() {
        let dir = std::env::temp_dir().join("mycraft-ini-test");
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("ext")).unwrap();
        fs::write(dir.join("ext").join("php_curl.dll"), "").unwrap();
        fs::write(dir.join("php.ini-development"), "[PHP]\n;extension_dir = \"ext\"\n;extension=curl\n;extension=gd\n;extension=ldap\n[Date]\n;date.timezone =\n").unwrap();
        let (ini, created) = ensure_ini(&dir).unwrap();
        let text = fs::read_to_string(ini).unwrap();
        assert!(created);
        assert!(text.contains("\r\nextension_dir = \"ext\"") && text.contains("\r\nextension=curl\r\n"));
        assert!(text.contains(";extension=gd") && text.contains(";extension=ldap"), "only shipped, listed ones");
        assert!(text.contains("date.timezone = UTC"));
        assert!(!ensure_ini(&dir).unwrap().1, "kept on second call");
    }
}
