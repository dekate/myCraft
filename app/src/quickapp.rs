//! Quick App: really scaffolds a project into `www\<name>` on a worker
//! thread and reports each step. Laravel uses Composer, WordPress the official
//! zip (SHA-1 checked), Next.js create-next-app; databases are created on the
//! running MySQL / MariaDB and wired into the project's config.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;

use crate::mysql::SqlClient;
use crate::state::LogLevel;

pub enum Note {
    Log(LogLevel, String),
    /// Finished; the text is the one-line result for the Quick App tab.
    Done(String),
    /// Re-check DNS and SSL trust (after hosts or the Root store changed).
    Refresh,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    Laravel,
    WordPress,
    NextJs,
    Static,
}

pub struct Job {
    pub name: String,
    pub hostname: String,
    pub kind: Kind,
    pub dir: PathBuf,
    pub sql: Option<SqlClient>,
    pub tx: Sender<Note>,
}

/// Project folder names that also work as virtual hosts and as a database name.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 48 && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

impl Job {
    fn log(&self, level: LogLevel, msg: impl Into<String>) {
        let _ = self.tx.send(Note::Log(level, msg.into()));
    }

    pub fn run(self) {
        let result = match self.kind {
            Kind::Laravel => self.laravel(),
            Kind::WordPress => self.wordpress(),
            Kind::NextJs => self.nextjs(),
            Kind::Static => self.static_site(),
        };
        let done = match result {
            Ok(summary) => {
                match self.create_nginx_vhost() {
                    Ok(path) => {
                        self.log(
                            LogLevel::Success,
                            format!("Generated Nginx virtual host: {}", path.file_name().unwrap_or_default().to_string_lossy()),
                        );
                    }
                    Err(e) => {
                        self.log(LogLevel::Warning, format!("Nginx virtual host: {e}"));
                    }
                }
                self.log(LogLevel::Success, &summary);
                summary
            }
            Err(e) => {
                self.log(LogLevel::Error, format!("{}: {e}", self.name));
                // Leave nothing half-made behind if the folder is still empty.
                let _ = std::fs::remove_dir(&self.dir);
                format!("Failed: {e}")
            }
        };
        let _ = self.tx.send(Note::Done(done));
    }

    /// Generate an Nginx virtual host configuration in `etc/nginx/sites-enabled/auto.<hostname>.conf`
    pub fn create_nginx_vhost(&self) -> Result<PathBuf, String> {
        let root = crate::mycraft_root();
        let sites_enabled = root.join("etc").join("nginx").join("sites-enabled");
        if !sites_enabled.exists() {
            std::fs::create_dir_all(&sites_enabled).map_err(|e| format!("cannot create sites-enabled dir: {e}"))?;
        }

        let web_root = if self.dir.join("public").is_dir() {
            self.dir.join("public")
        } else {
            self.dir.clone()
        };
        let root_str = web_root.to_string_lossy().replace('\\', "/");

        let laragon_crt = root.join("etc").join("ssl").join("laragon.crt");
        let (crt_str, key_str) = if laragon_crt.is_file() {
            (
                root.join("etc/ssl/laragon.crt").to_string_lossy().replace('\\', "/"),
                root.join("etc/ssl/laragon.key").to_string_lossy().replace('\\', "/"),
            )
        } else {
            (
                root.join("etc/ssl/mycraft-ca.crt").to_string_lossy().replace('\\', "/"),
                root.join("etc/ssl/mycraft-ca.key").to_string_lossy().replace('\\', "/"),
            )
        };

        let conf = format!(
r#"server {{
    listen 80;
    listen 443 ssl;
    server_name {hostname} *.{hostname};
    root "{root_str}";
    
    index index.html index.htm index.php;
 
    location / {{
        try_files $uri $uri/ /index.php$is_args$args;
		autoindex on;
    }}
    
    location ~ \.php$ {{
        include snippets/fastcgi-php.conf;
        fastcgi_pass php_upstream;		
        #fastcgi_pass unix:/run/php/php7.0-fpm.sock;
    }}

    # Enable SSL
    ssl_certificate "{crt_str}";
    ssl_certificate_key "{key_str}";
    ssl_session_timeout 5m;
    ssl_protocols TLSv1 TLSv1.1 TLSv1.2;
    ssl_ciphers ALL:!ADH:!EXPORT56:RC4+RSA:+HIGH:+MEDIUM:+LOW:+SSLv3:+EXP;
    ssl_prefer_server_ciphers on;
	
    charset utf-8;
	
    location = /favicon.ico {{ access_log off; log_not_found off; }}
    location = /robots.txt  {{ access_log off; log_not_found off; }}
    location ~ /\.ht {{
        deny all;
    }}
}}

# This file is auto-generated.
# If you want Laragon to respect your changes, just remove the [auto.] prefix
"#,
            hostname = self.hostname,
            root_str = root_str,
            crt_str = crt_str,
            key_str = key_str,
        );

        let out_file = sites_enabled.join(format!("auto.{}.conf", self.hostname));
        std::fs::write(&out_file, conf).map_err(|e| format!("cannot write {}: {e}", out_file.display()))?;
        Ok(out_file)
    }

    fn database(&self) -> Option<&SqlClient> {
        let sql = self.sql.as_ref()?;
        match sql.create_database(&self.name) {
            Ok(()) => {
                self.log(LogLevel::Success, format!("Created database `{}` on 127.0.0.1:{}.", self.name, sql.port));
                Some(sql)
            }
            Err(e) => {
                self.log(LogLevel::Error, format!("Could not create database `{}`: {e}", self.name));
                None
            }
        }
    }

    fn laravel(&self) -> Result<String, String> {
        let php = newest_php()?;
        let composer = crate::packages::composer_phar().ok_or("Composer isn't installed: ⚙ PHP → Install → composer")?;
        self.log(LogLevel::Info, format!("composer create-project laravel/laravel {} (PHP {})…", self.name, php.display()));
        let mut cmd = Command::new(&php);
        cmd.arg(&composer)
            .args(["create-project", "laravel/laravel"])
            .arg(&self.dir)
            .args(["--no-interaction", "--prefer-dist", "--no-progress"])
            .env("COMPOSER_NO_INTERACTION", "1");
        run(&mut cmd, self.dir.parent().unwrap_or(&self.dir))?;

        let summary = match self.database() {
            Some(sql) => {
                let env = self.dir.join(".env");
                let text = std::fs::read_to_string(&env).map_err(|e| format!("{}: {e}", env.display()))?;
                let pw = sql.password.clone().unwrap_or_default();
                let port = sql.port.to_string();
                let text = set_env(
                    &text,
                    &[
                        ("DB_CONNECTION", "mysql"),
                        ("DB_HOST", "127.0.0.1"),
                        ("DB_PORT", &port),
                        ("DB_DATABASE", &self.name),
                        ("DB_USERNAME", "root"),
                        ("DB_PASSWORD", &pw),
                    ],
                );
                std::fs::write(&env, text).map_err(|e| format!("{}: {e}", env.display()))?;
                self.log(LogLevel::Info, "Wrote the MySQL settings to .env; running php artisan migrate…");
                let mut migrate = Command::new(&php);
                migrate.args(["artisan", "migrate", "--force", "--no-interaction"]);
                match run(&mut migrate, &self.dir) {
                    Ok(_) => format!("Laravel ready at http://{} (database `{}`)", self.hostname, self.name),
                    Err(e) => {
                        self.log(LogLevel::Warning, format!("migrate failed: {e}"));
                        format!("Laravel created at http://{}; run php artisan migrate yourself", self.hostname)
                    }
                }
            }
            None => {
                self.log(LogLevel::Warning, "MySQL / MariaDB isn't running, so the project keeps Laravel's SQLite database.");
                format!("Laravel ready at http://{} (SQLite)", self.hostname)
            }
        };
        Ok(summary)
    }

    fn wordpress(&self) -> Result<String, String> {
        // Staged next to the project (same drive, so the final moves are renames);
        // the leading dot keeps it out of project lists.
        let tmp = self.dir.with_file_name(format!(".mycraft-wp-{}", self.name));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
        let result = (|| {
            let zip = tmp.join("latest.zip");
            self.log(LogLevel::Info, "Downloading https://wordpress.org/latest.zip…");
            crate::packages::download("https://wordpress.org/latest.zip", &zip)?;
            let want = crate::packages::agent()
                .get("https://wordpress.org/latest.zip.sha1")
                .call()
                .map_err(|e| format!("could not fetch the checksum: {e}"))?
                .into_string()
                .map_err(|e| e.to_string())?;
            let got = crate::packages::file_hash(&zip, "SHA1")?;
            if want.trim().to_ascii_lowercase() != got {
                return Err("checksum mismatch for latest.zip".to_string());
            }
            let mut tar = Command::new("tar");
            tar.arg("-xf").arg(&zip).arg("-C").arg(&tmp);
            run(&mut tar, &tmp)?;
            let unpacked = tmp.join("wordpress");
            for e in std::fs::read_dir(&unpacked).map_err(|e| e.to_string())?.flatten() {
                std::fs::rename(e.path(), self.dir.join(e.file_name())).map_err(|e| e.to_string())?;
            }
            Ok(())
        })();
        let _ = std::fs::remove_dir_all(&tmp);
        result?;
        self.log(LogLevel::Success, format!("Unpacked WordPress into {}.", self.dir.display()));

        match self.database() {
            Some(sql) => {
                let sample = std::fs::read_to_string(self.dir.join("wp-config-sample.php")).map_err(|e| e.to_string())?;
                let mut config = sample
                    .replace("database_name_here", &self.name)
                    .replace("username_here", "root")
                    .replace("password_here", sql.password.as_deref().unwrap_or(""))
                    .replace("'localhost'", &format!("'127.0.0.1:{}'", sql.port));
                while config.contains("put your unique phrase here") {
                    config = config.replacen("put your unique phrase here", &random_salt(), 1);
                }
                std::fs::write(self.dir.join("wp-config.php"), config).map_err(|e| e.to_string())?;
                self.log(LogLevel::Success, "Wrote wp-config.php with the database settings and fresh salts.");
                Ok(format!("WordPress ready: open http://{} to finish the install", self.hostname))
            }
            None => {
                self.log(LogLevel::Warning, "MySQL / MariaDB isn't running: no database or wp-config.php yet. Start it, then open the site.");
                Ok(format!("WordPress unpacked at http://{} (needs a database)", self.hostname))
            }
        }
    }

    fn nextjs(&self) -> Result<String, String> {
        let node = newest_node()?;
        self.log(LogLevel::Info, format!("npx create-next-app {} (Node {})…", self.name, node.display()));
        let path = format!("{};{}", node.display(), std::env::var("PATH").unwrap_or_default());
        let mut cmd = Command::new("cmd.exe");
        cmd.arg("/c")
            .arg(node.join("npx.cmd"))
            .args(["--yes", "create-next-app@latest"])
            .arg(&self.name)
            .args(["--yes", "--use-npm", "--disable-git"])
            .env("PATH", path);
        run(&mut cmd, self.dir.parent().unwrap_or(&self.dir))?;
        Ok(format!("Next.js app created in {}: run `npm run dev` there (virtual host: http://{})", self.dir.display(), self.hostname))
    }

    fn static_site(&self) -> Result<String, String> {
        let html = format!(
            "<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n  <meta charset=\"utf-8\">\n  <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\n  <title>{n}</title>\n  <link rel=\"stylesheet\" href=\"style.css\">\n</head>\n<body>\n  <main>\n    <h1>{n}</h1>\n    <p>Edit <code>index.html</code> to get started.</p>\n  </main>\n  <script src=\"app.js\"></script>\n</body>\n</html>\n",
            n = self.name
        );
        let css = "body { font-family: system-ui, sans-serif; margin: 0; display: grid; place-items: center; min-height: 100vh; }\n";
        for (file, text) in [("index.html", html.as_str()), ("style.css", css), ("app.js", "console.log('ready');\n")] {
            std::fs::write(self.dir.join(file), text).map_err(|e| format!("{file}: {e}"))?;
        }
        Ok(format!("Static site ready at http://{}", self.hostname))
    }
}

/// Run a tool to completion; on failure return the last lines of its output.
fn run(cmd: &mut Command, cwd: &Path) -> Result<String, String> {
    cmd.current_dir(cwd).stdin(Stdio::null());
    let out = crate::hide_console(cmd).output().map_err(|e| format!("could not run {:?}: {e}", cmd.get_program()))?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    if out.status.success() {
        Ok(text)
    } else {
        let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        Err(lines[lines.len().saturating_sub(3)..].join(" | "))
    }
}

/// php.exe of the newest installed PHP, with a php.ini that loads the usual extensions.
fn newest_php() -> Result<PathBuf, String> {
    let bin = crate::mycraft_root().join("bin").join("php");
    let dir = crate::php_update::newest(&bin).ok_or("no PHP installed: ⚙ PHP → Install")?;
    crate::php_update::ensure_ini(&dir)?;
    Ok(dir.join("php.exe"))
}

/// Folder of the newest installed Node.js (the one holding node.exe).
pub fn newest_node() -> Result<PathBuf, String> {
    let bin = crate::packages::bin_dir("nodejs");
    crate::packages::installed("nodejs")
        .into_iter()
        .map(|d| bin.join(d))
        .find(|d| d.join("node.exe").is_file() && d.join("npx.cmd").is_file())
        .ok_or_else(|| "no Node.js installed: ⚙ Node.js → Install".to_string())
}

/// Set `KEY=value` lines in a .env, uncommenting `# KEY=` ones; append missing keys.
pub fn set_env(text: &str, pairs: &[(&str, &str)]) -> String {
    let mut lines: Vec<String> = text.lines().map(String::from).collect();
    for (key, value) in pairs {
        let line = format!("{key}={value}");
        let pos = lines.iter().position(|l| {
            let t = l.trim_start().trim_start_matches('#').trim_start();
            t.strip_prefix(key).is_some_and(|r| r.trim_start().starts_with('='))
        });
        match pos {
            Some(i) => lines[i] = line,
            None => lines.push(line),
        }
    }
    lines.join("\n") + "\n"
}

fn random_salt() -> String {
    use std::hash::{BuildHasher, Hasher};
    let charset = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789!@#%^&*()-_[]{}<>~+=,.;:/?|";
    let mut out = String::with_capacity(64);
    while out.len() < 64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_usize(out.len());
        let mut v = h.finish();
        for _ in 0..8 {
            out.push(charset[(v % charset.len() as u64) as usize] as char);
            v /= charset.len() as u64;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_lines_are_set_and_uncommented() {
        let env = "APP_NAME=Laravel\nDB_CONNECTION=sqlite\n# DB_HOST=127.0.0.1\n# DB_DATABASE=laravel\n";
        let out = set_env(env, &[("DB_CONNECTION", "mysql"), ("DB_HOST", "127.0.0.1"), ("DB_DATABASE", "shop"), ("DB_PASSWORD", "")]);
        assert_eq!(out, "APP_NAME=Laravel\nDB_CONNECTION=mysql\nDB_HOST=127.0.0.1\nDB_DATABASE=shop\nDB_PASSWORD=\n");
    }

    /// Real downloads: `cargo test -- --ignored live_quickapp --nocapture`
    #[test]
    #[ignore]
    fn live_quickapp() {
        let www = std::env::temp_dir().join("mycraft-live-quickapp");
        let _ = std::fs::remove_dir_all(&www);
        for (name, kind, file) in [("site", Kind::Static, "index.html"), ("blog", Kind::WordPress, "wp-config-sample.php")] {
            let dir = www.join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let (tx, rx) = std::sync::mpsc::channel();
            Job {
                name: name.into(),
                hostname: format!("{name}.go"),
                kind,
                dir: dir.clone(),
                sql: None,
                tx,
            }
            .run();
            for n in rx.try_iter() {
                match n {
                    Note::Log(_, m) | Note::Done(m) => println!("{name}: {m}"),
                    Note::Refresh => {}
                }
            }
            assert!(dir.join(file).is_file(), "{name}: no {file}");
        }
    }

    #[test]
    fn names() {
        assert!(valid_name("my-shop_2"));
        assert!(!valid_name("My Shop") && !valid_name("../x") && !valid_name(""));
    }

    #[test]
    fn nginx_vhost_generation() {
        let tmp = std::env::temp_dir().join(format!("mycraft-vhost-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let project_dir = tmp.join("myblog");
        let public_dir = project_dir.join("public");
        std::fs::create_dir_all(&public_dir).unwrap();
        let (tx, _rx) = std::sync::mpsc::channel();
        let job = Job {
            name: "myblog".into(),
            hostname: "myblog.test".into(),
            kind: Kind::Laravel,
            dir: project_dir,
            sql: None,
            tx,
        };
        let conf_path = job.create_nginx_vhost().expect("should create nginx vhost");
        assert!(conf_path.is_file(), "vhost file should exist");
        let content = std::fs::read_to_string(&conf_path).expect("should read conf");
        assert!(content.contains("server_name myblog.test *.myblog.test;"));
        assert!(content.contains("myblog/public"));
        // Clean up test conf
        let _ = std::fs::remove_file(&conf_path);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
