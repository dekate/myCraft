//! Downloadable packages from `<root>\usr\packages.conf`. Each entry installs
//! into `bin\<section>\`, e.g. `[mysql]` + `mariadb-11.8=auto` →
//! `bin\mysql\mariadb-11.8.9-winx64`. `auto` resolves the newest release from
//! the project's official feed (with its SHA-256); otherwise the source is an
//! https zip / tar.gz URL, optionally followed by `sha256=<hex>`.

use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Written to `usr\packages.conf` on first run; the user's copy wins after that.
pub const DEFAULT_CONF: &str = "\
# myCraft packages: one per line, `name=source`, installed into bin\\<section>\\.
# source is `auto` (newest release from the official feed, checksum-verified;
# works for php-X.Y, mariadb-X.Y, node-NN, heidisql and composer) or an https .zip / .tar.gz URL,
# optionally followed by ` sha256=<hex>`.

[php]
php-8.4=auto
php-8.3=auto
php-8.2=auto

[mysql]
mariadb-11.8=auto
mariadb-11.4=auto

[nodejs]
node-24=auto
node-22=auto

[heidisql]
heidisql=auto

# Needed by Quick App → Laravel.
[composer]
composer=auto

# Download-only for now: myCraft installs these into bin\\ but can't start them yet.
# No checksums are published for them, so bump the versions here yourself.
[postgresql]
postgresql-18=https://get.enterprisedb.com/postgresql/postgresql-18.6-1-windows-x64-binaries.zip
postgresql-17=https://get.enterprisedb.com/postgresql/postgresql-17.11-1-windows-x64-binaries.zip

# Redis has no official Windows build; this is the community redis-windows port.
# No checksum is published, so bump the version here yourself.
[redis]
redis-8=https://github.com/redis-windows/redis-windows/releases/download/8.10.3/Redis-8.10.3-Windows-x64-msys2.zip

# Memcached has no maintained Windows download. Add a line here if you have one:
# [memcached]
# memcached=https://example.org/memcached-win64.zip
";

#[derive(Clone, Debug, PartialEq)]
pub struct Package {
    pub section: String,
    pub name: String,
    pub source: Source,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Source {
    Auto,
    Url { url: String, sha256: Option<String> },
}

fn safe_name(s: &str) -> bool {
    !s.is_empty() && !s.contains("..") && s.chars().all(|c| c.is_ascii_alphanumeric() || "-._".contains(c))
}

pub fn parse(text: &str) -> Vec<Package> {
    let mut section = String::new();
    let mut out = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(s) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = s.trim().to_ascii_lowercase();
            continue;
        }
        let Some((name, src)) = line.split_once('=') else { continue };
        let (name, src) = (name.trim().trim_start_matches('*'), src.trim());
        // No [section] yet: the name prefix picks the folder (mysql-5.7 → bin\mysql).
        let section = if section.is_empty() { name.split('-').next().unwrap_or("").to_ascii_lowercase() } else { section.clone() };
        if !safe_name(&section) || !safe_name(name) {
            continue;
        }
        let source = if src.eq_ignore_ascii_case("auto") {
            Source::Auto
        } else {
            let mut parts = src.split_whitespace();
            let url = parts.next().unwrap_or("").to_string();
            let sha256 = parts.find_map(|p| p.strip_prefix("sha256=")).map(str::to_ascii_lowercase);
            if !url.starts_with("https://") {
                continue;
            }
            Source::Url { url, sha256 }
        };
        out.push(Package { section, name: name.to_string(), source });
    }
    out
}

pub fn conf_path() -> PathBuf {
    crate::mycraft_root().join("usr").join("packages.conf")
}

/// The package list, creating the default file on first run.
pub fn load() -> Vec<Package> {
    let path = conf_path();
    if !path.exists() {
        let _ = fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
        let _ = fs::write(&path, DEFAULT_CONF);
    }
    parse(&fs::read_to_string(&path).unwrap_or_else(|_| DEFAULT_CONF.to_string()))
}

pub fn bin_dir(section: &str) -> PathBuf {
    crate::mycraft_root().join("bin").join(section)
}

/// Installed folders in `bin\<section>`, newest-looking first.
pub fn installed(section: &str) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(bin_dir(section))
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().to_string())
                // Skip partial downloads and backups (`node-v22.11.0-win-x64.bak`).
                .filter(|n| !n.starts_with('.') && ![".part", ".bak", ".old"].iter().any(|s| n.to_ascii_lowercase().ends_with(s)))
                .collect()
        })
        .unwrap_or_default();
    v.sort_by_key(|n| std::cmp::Reverse(version_key(n)));
    v
}

/// Numeric parts of a folder name, for sorting `node-v24.1.0` above `node-v22.9.0`.
fn version_key(name: &str) -> Vec<u32> {
    name.split(|c: char| !c.is_ascii_digit()).filter_map(|p| p.parse().ok()).collect()
}

/// What to download for a package.
#[derive(Clone, Debug, PartialEq)]
pub struct Download {
    pub url: String,
    pub file_name: String,
    pub sha256: Option<String>,
}

pub fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(60))
        .build()
}

fn get_json(url: &str) -> Result<Value, String> {
    agent().get(url).call().map_err(|e| format!("{url}: {e}"))?.into_json().map_err(|e| format!("{url}: {e}"))
}

fn get_text(url: &str) -> Result<String, String> {
    agent().get(url).call().map_err(|e| format!("{url}: {e}"))?.into_string().map_err(|e| format!("{url}: {e}"))
}

pub fn resolve(p: &Package) -> Result<Download, String> {
    let url = match &p.source {
        Source::Url { url, sha256 } => {
            let file_name = url.split(['?', '#']).next().unwrap_or("").rsplit('/').next().unwrap_or("").to_string();
            return Ok(Download { url: url.clone(), file_name, sha256: sha256.clone() });
        }
        Source::Auto => &p.name,
    };
    if url == "heidisql" {
        return resolve_heidisql();
    }
    if url == "composer" {
        return resolve_composer();
    }
    let (kind, line) = url.split_once('-').ok_or_else(|| format!("{}: `auto` needs a name like php-8.4", p.name))?;
    match kind {
        "php" => resolve_php(line),
        "node" => resolve_node(line),
        "mariadb" => resolve_mariadb(line),
        _ => Err(format!("{}: `auto` only knows php-, node-, mariadb-, heidisql and composer; give a URL instead", p.name)),
    }
}

/// Newest thread-safe x64 build of a PHP line from windows.php.net.
fn resolve_php(line: &str) -> Result<Download, String> {
    let json = get_json(&format!("{}releases.json", crate::php_update::RELEASES))?;
    let builds = json.get(line).and_then(Value::as_object).ok_or_else(|| format!("PHP {line} is not on windows.php.net (end of life?)"))?;
    let (_, build) = builds
        .iter()
        .filter(|(k, _)| k.starts_with("ts-") && k.ends_with("-x64"))
        .max_by(|a, b| a.0.cmp(b.0))
        .ok_or_else(|| format!("no x64 build for PHP {line}"))?;
    let zip = &build["zip"];
    let file_name = zip["path"].as_str().ok_or("bad releases.json")?.to_string();
    Ok(Download {
        url: format!("{}{file_name}", crate::php_update::RELEASES),
        file_name,
        sha256: zip["sha256"].as_str().map(str::to_ascii_lowercase),
    })
}

/// Newest `vNN.x` Windows x64 zip from nodejs.org, checksum from SHASUMS256.txt.
fn resolve_node(major: &str) -> Result<Download, String> {
    let index = get_json("https://nodejs.org/dist/index.json")?;
    let prefix = format!("v{major}.");
    let version = index
        .as_array()
        .and_then(|a| a.iter().filter_map(|r| r["version"].as_str()).find(|v| v.starts_with(&prefix)))
        .ok_or_else(|| format!("no Node.js {major} release found"))?;
    let file_name = format!("node-{version}-win-x64.zip");
    let sums = get_text(&format!("https://nodejs.org/dist/{version}/SHASUMS256.txt"))?;
    let sha256 = sums
        .lines()
        .find(|l| l.ends_with(&format!(" {file_name}")))
        .and_then(|l| l.split_whitespace().next())
        .map(str::to_ascii_lowercase);
    Ok(Download { url: format!("https://nodejs.org/dist/{version}/{file_name}"), file_name, sha256 })
}

/// Newest 64-bit portable HeidiSQL: the download page on heidisql.com lists the
/// zip with its sha256; GitHub's API (rate-limited for anonymous use) is the fallback.
fn resolve_heidisql() -> Result<Download, String> {
    match get_text("https://www.heidisql.com/download.php").map(|page| heidisql_from_page(&page)) {
        Ok(Some(d)) => Ok(d),
        _ => heidisql_from_github(),
    }
}

/// `<a href="…_64_Portable.zip">` followed by its `data-digest="sha256:…"`.
fn heidisql_from_page(page: &str) -> Option<Download> {
    let end = page.find("_64_Portable.zip\"")? + "_64_Portable.zip".len();
    let start = page[..end].rfind("href=\"")? + "href=\"".len();
    let url = &page[start..end];
    let rest = &page[end..];
    let d = rest.find("data-digest=\"sha256:")? + "data-digest=\"sha256:".len();
    let sha = rest.get(d..d + 64)?.to_ascii_lowercase();
    if !url.starts_with("https://") || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(Download { url: url.to_string(), file_name: url.rsplit('/').next()?.to_string(), sha256: Some(sha) })
}

fn heidisql_from_github() -> Result<Download, String> {
    let rel = get_json("https://api.github.com/repos/HeidiSQL/HeidiSQL/releases/latest")?;
    let asset = rel["assets"]
        .as_array()
        .and_then(|a| a.iter().find(|a| a["name"].as_str().is_some_and(|n| n.ends_with("_64_Portable.zip"))))
        .ok_or("no 64-bit portable HeidiSQL in the latest release")?;
    Ok(Download {
        url: asset["browser_download_url"].as_str().ok_or("bad GitHub release")?.to_string(),
        file_name: asset["name"].as_str().ok_or("bad GitHub release")?.to_string(),
        sha256: asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).map(str::to_ascii_lowercase),
    })
}

/// Newest stable composer.phar from getcomposer.org, with its sha256sum.
/// Saved as `composer-<version>.phar` → `bin\composer\composer-<version>\composer.phar`.
fn resolve_composer() -> Result<Download, String> {
    let versions = get_json("https://getcomposer.org/versions")?;
    let stable = &versions["stable"][0];
    let (path, version) = (
        stable["path"].as_str().ok_or("bad getcomposer.org/versions")?,
        stable["version"].as_str().ok_or("bad getcomposer.org/versions")?,
    );
    let url = format!("https://getcomposer.org{path}");
    let sum = get_text(&format!("{url}.sha256sum"))?;
    Ok(Download {
        url,
        file_name: format!("composer-{version}.phar"),
        sha256: sum.split_whitespace().next().map(str::to_ascii_lowercase),
    })
}

/// composer.phar: `bin\composer\composer.phar` (Laragon layout) or the newest
/// `bin\composer\<version>\composer.phar`.
pub fn composer_phar() -> Option<PathBuf> {
    let bin = bin_dir("composer");
    std::iter::once(bin.join("composer.phar"))
        .chain(installed("composer").into_iter().map(|d| bin.join(d).join("composer.phar")))
        .find(|p| p.is_file())
}

/// Newest release of a MariaDB series from downloads.mariadb.org.
fn resolve_mariadb(series: &str) -> Result<Download, String> {
    let json = get_json(&format!("https://downloads.mariadb.org/rest-api/mariadb/{series}/"))?;
    let releases = json["releases"].as_object().ok_or_else(|| format!("MariaDB {series} not found"))?;
    let (_, rel) = releases
        .iter()
        .max_by_key(|(k, _)| version_key(k))
        .ok_or_else(|| format!("MariaDB {series} has no releases"))?;
    let file = rel["files"]
        .as_array()
        .and_then(|f| f.iter().find(|f| f["file_name"].as_str().is_some_and(|n| n.ends_with("-winx64.zip"))))
        .ok_or_else(|| format!("MariaDB {series}: no Windows zip"))?;
    Ok(Download {
        url: file["file_download_url"].as_str().ok_or("bad MariaDB feed")?.replacen("http://", "https://", 1),
        file_name: file["file_name"].as_str().ok_or("bad MariaDB feed")?.to_string(),
        sha256: file["checksum"]["sha256sum"].as_str().map(str::to_ascii_lowercase),
    })
}

/// Download `d` into `parent`, verify, unpack, and return the new folder:
/// `parent\<archive's single top folder>` or `parent\<archive name>`.
/// Already installed → returns it without downloading.
pub fn install(d: &Download, parent: &Path) -> Result<PathBuf, String> {
    // A lone `tool-<version>.phar` goes to parent\tool-<version>\tool.phar.
    if let Some(stem) = d.file_name.strip_suffix(".phar").filter(|s| safe_name(s)) {
        let tool = stem.split('-').next().unwrap_or(stem);
        let dest = parent.join(stem);
        let file = dest.join(format!("{tool}.phar"));
        if file.is_file() {
            return Ok(dest);
        }
        fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
        let part = dest.join(format!("{tool}.phar.part"));
        let result = download(&d.url, &part).and_then(|_| match &d.sha256 {
            Some(want) if &sha256(&part)? != want => Err(format!("checksum mismatch for {}", d.file_name)),
            _ => fs::rename(&part, &file).map_err(|e| e.to_string()),
        });
        let _ = fs::remove_file(&part);
        if result.is_err() {
            let _ = fs::remove_dir(&dest);
        }
        return result.map(|_| dest);
    }
    let stem = d
        .file_name
        .strip_suffix(".zip")
        .or_else(|| d.file_name.strip_suffix(".tar.gz"))
        .or_else(|| d.file_name.strip_suffix(".tgz"))
        .filter(|s| safe_name(s))
        .ok_or_else(|| format!("{} is not a .zip or .tar.gz download", d.url))?;
    if let Some(s) = &d.sha256 {
        if s.len() != 64 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("bad sha256 for {}", d.file_name));
        }
    }
    if parent.join(stem).is_dir() {
        return Ok(parent.join(stem));
    }
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;

    let archive = parent.join(format!("{}.part", d.file_name));
    let staging = parent.join(format!("{stem}.part"));
    let result = (|| {
        download(&d.url, &archive)?;

        if let Some(want) = &d.sha256 {
            let got = sha256(&archive)?;
            if &got != want {
                return Err(format!("checksum mismatch for {} (got {got})", d.file_name));
            }
        }

        let _ = fs::remove_dir_all(&staging);
        fs::create_dir_all(&staging).map_err(|e| e.to_string())?;
        let out = crate::hide_console(Command::new("tar").arg("-xf").arg(&archive).arg("-C").arg(&staging))
            .output()
            .map_err(|e| format!("could not run tar: {e}"))?;
        if !out.status.success() {
            return Err(format!("unpacking failed: {}", String::from_utf8_lossy(&out.stderr).trim()));
        }

        // Archives that wrap everything in one versioned folder (Node, MariaDB)
        // keep its name; an unversioned one (PostgreSQL's `pgsql`) is named after
        // the archive so versions don't collide.
        let entries: Vec<PathBuf> = fs::read_dir(&staging).map_err(|e| e.to_string())?.flatten().map(|e| e.path()).collect();
        let (from, name) = match entries.as_slice() {
            [only] if only.is_dir() => {
                let inner = only.file_name().unwrap_or_default().to_string_lossy().to_string();
                let name = if inner.chars().any(|c| c.is_ascii_digit()) { inner } else { stem.to_string() };
                (only.clone(), name)
            }
            _ => (staging.clone(), stem.to_string()),
        };
        let dest = parent.join(&name);
        if dest.exists() {
            return Ok(dest);
        }
        fs::rename(&from, &dest).map_err(|e| e.to_string())?;
        Ok(dest)
    })();
    let _ = fs::remove_file(&archive);
    let _ = fs::remove_dir_all(&staging);
    result
}

pub fn download(url: &str, to: &Path) -> Result<(), String> {
    let res = agent().get(url).call().map_err(|e| format!("download failed: {e}"))?;
    let mut file = fs::File::create(to).map_err(|e| e.to_string())?;
    std::io::copy(&mut res.into_reader(), &mut file).map_err(|e| format!("download failed: {e}"))?;
    Ok(())
}

pub fn sha256(file: &Path) -> Result<String, String> {
    file_hash(file, "SHA256")
}

/// Lowercase hex digest via certutil (`SHA1`, `SHA256`, …).
pub fn file_hash(file: &Path, algo: &str) -> Result<String, String> {
    let out = crate::hide_console(Command::new("certutil").arg("-hashfile").arg(file).arg(algo))
        .output()
        .map_err(|e| format!("could not run certutil: {e}"))?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(|l| l.replace(' ', "").to_ascii_lowercase())
        .find(|l| l.len() >= 40 && l.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or_else(|| "could not hash the download".to_string())
}

#[derive(Default)]
struct Shared {
    busy: Option<String>,
    message: String,
}

/// One install at a time on a worker thread; the UI polls `status()`.
#[derive(Clone, Default)]
pub struct Installer {
    shared: Arc<Mutex<Shared>>,
}

impl Installer {
    /// (package being installed, last message)
    pub fn status(&self) -> (Option<String>, String) {
        let s = self.shared.lock().unwrap();
        (s.busy.clone(), s.message.clone())
    }

    pub fn install(&self, p: Package) {
        {
            let mut s = self.shared.lock().unwrap();
            if s.busy.is_some() {
                return;
            }
            s.busy = Some(p.name.clone());
            s.message = format!("Downloading {}…", p.name);
        }
        let shared = self.shared.clone();
        std::thread::spawn(move || {
            let result = resolve(&p).and_then(|d| {
                let unverified = d.sha256.is_none();
                install(&d, &bin_dir(&p.section)).map(|dir| (dir, unverified))
            });
            // A fresh PHP has no php.ini, so no extensions: give it a working one.
            if let (Ok((dir, _)), "php") = (&result, p.section.as_str()) {
                let _ = crate::php_update::ensure_ini(dir);
            }
            let mut s = shared.lock().unwrap();
            s.message = match result {
                Ok((dir, unverified)) => format!(
                    "Installed {} to {}{}",
                    p.name,
                    dir.display(),
                    if unverified { " (no checksum given in packages.conf)" } else { "" }
                ),
                Err(e) => format!("{} failed: {e}", p.name),
            };
            s.busy = None;
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_default_conf() {
        let p = parse(DEFAULT_CONF);
        assert_eq!(p[0], Package { section: "php".into(), name: "php-8.4".into(), source: Source::Auto });
        assert!(p.iter().any(|p| p.section == "mysql" && p.name == "mariadb-11.8"));
        assert!(p.iter().any(|p| p.section == "postgresql" && matches!(p.source, Source::Url { .. })));
        assert!(p.iter().all(|p| p.section != "memcached"), "commented out");
    }

    #[test]
    fn parses_urls_and_drops_unsafe_lines() {
        let p = parse(
            "[apps]\nfoo=https://x.org/foo-1.0.zip sha256=ABCD\nbar=http://insecure.org/bar.zip\n../evil=https://x.org/e.zip\nnoeq\n[../x]\nbaz=auto",
        );
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].source, Source::Url { url: "https://x.org/foo-1.0.zip".into(), sha256: Some("abcd".into()) });
    }

    #[test]
    fn sectionless_lines_use_name_prefix() {
        let p = parse("# MySQL\nmysql-5.7=https://dev.mysql.com/mysql-5.7.39-winx64.zip\n*yarn=https://x.org/yarn.tar.gz\n------\n");
        assert_eq!((p[0].section.as_str(), p[1].section.as_str(), p[1].name.as_str()), ("mysql", "yarn", "yarn"));
    }

    #[test]
    fn install_rejects_bad_names_and_hashes() {
        let d = |f: &str, s: Option<&str>| Download { url: "https://x".into(), file_name: f.into(), sha256: s.map(Into::into) };
        let tmp = std::env::temp_dir();
        assert!(install(&d("setup.exe", None), &tmp).is_err());
        assert!(install(&d("..\\x.zip", None), &tmp).is_err());
        assert!(install(&d("ok.zip", Some("zz")), &tmp).is_err());
    }

    /// Hits the real feeds: `cargo test -- --ignored live_feeds --nocapture`.
    #[test]
    #[ignore]
    fn live_feeds() {
        for name in ["php-8.2", "node-22", "mariadb-11.8", "heidisql", "composer"] {
            let d = resolve(&Package { section: "x".into(), name: name.into(), source: Source::Auto }).unwrap();
            println!("{name}: {} sha256={:?}", d.url, d.sha256);
            assert!(d.sha256.is_some_and(|s| s.len() == 64));
        }
    }

    #[test]
    fn heidisql_page_parse() {
        let page = r#"<a href="https://github.com/x/HeidiSQL_12.22_Setup.exe">Setup</a><a data-digest="sha256:AAAA">
<li><a href="https://github.com/HeidiSQL/HeidiSQL/releases/download/v12.22/HeidiSQL_12.22_64_Portable.zip">Windows x64 Portable</a>
<a class="checksum" data-digest="sha256:7B36821FA3E16CC37B511D41747EFA4C8B980D1E3FA937E52C5931BCC4EE79B4">"#;
        let d = heidisql_from_page(page).unwrap();
        assert_eq!(d.file_name, "HeidiSQL_12.22_64_Portable.zip");
        assert_eq!(d.sha256.as_deref(), Some("7b36821fa3e16cc37b511d41747efa4c8b980d1e3fa937e52c5931bcc4ee79b4"));
    }

    #[test]
    fn sorts_newest_first() {
        let mut v = vec!["node-v22.9.0-win-x64", "node-v24.1.0-win-x64", "node-v22.10.0-win-x64"];
        v.sort_by_key(|n| std::cmp::Reverse(version_key(n)));
        assert_eq!(v, ["node-v24.1.0-win-x64", "node-v22.10.0-win-x64", "node-v22.9.0-win-x64"]);
    }
}
