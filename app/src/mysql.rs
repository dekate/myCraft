//! Runs MariaDB / MySQL from `bin\mysql\<install>`. Data lives in
//! `data\<family>-<major.minor>` (e.g. `data\mariadb-11.8`), so patch upgrades
//! keep their databases; it is initialized on first start. Settings come from
//! `conf\my.ini` (written on first start); myCraft passes datadir, port and
//! bind-address itself. Server output goes to `logs\mysql.log`.

use std::fs;
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub const DEFAULT_MY_INI: &str = "\
# myCraft MySQL / MariaDB settings. datadir, port and bind-address are set
# by myCraft when it starts the server, so they are not listed here.
[mysqld]
character-set-server=utf8mb4
collation-server=utf8mb4_unicode_ci
max_allowed_packet=64M
innodb_buffer_pool_size=256M

[client]
default-character-set=utf8mb4
";

/// One install under bin\mysql and the files it needs.
#[derive(Clone, Debug)]
pub struct Install {
    pub dir: PathBuf,
    pub is_mysql: bool,
    pub data_dir: PathBuf,
}

impl Install {
    pub fn new(root: &Path, folder: &str) -> Result<Install, String> {
        let dir = root.join("bin").join("mysql").join(folder);
        let key = data_key(folder).ok_or_else(|| format!("can't tell the version of {folder}"))?;
        let inst = Install { is_mysql: folder.to_ascii_lowercase().starts_with("mysql"), data_dir: root.join("data").join(key), dir };
        if !inst.server().is_file() {
            return Err(format!("{} has no mysqld.exe", inst.dir.display()));
        }
        Ok(inst)
    }

    fn tool(&self, names: &[&str]) -> PathBuf {
        let bin = self.dir.join("bin");
        names.iter().map(|n| bin.join(n)).find(|p| p.is_file()).unwrap_or_else(|| bin.join(names[names.len() - 1]))
    }
    pub fn server(&self) -> PathBuf {
        self.tool(&["mariadbd.exe", "mysqld.exe"])
    }
    pub fn admin(&self) -> PathBuf {
        self.tool(&["mariadb-admin.exe", "mysqladmin.exe"])
    }
    pub fn client(&self) -> PathBuf {
        self.tool(&["mariadb.exe", "mysql.exe"])
    }
}

/// `mariadb-11.8.9-winx64` → `mariadb-11.8`; `mysql-8.4.9-winx64` → `mysql-8.4`.
pub fn data_key(folder: &str) -> Option<String> {
    let mut parts = folder.split('-');
    let family = parts.next()?.to_ascii_lowercase();
    let version = parts.next()?;
    let mm: Vec<&str> = version.split('.').take(2).collect();
    if mm.len() != 2 || mm.iter().any(|p| p.parse::<u32>().is_err()) || family.is_empty() {
        return None;
    }
    Some(format!("{family}-{}", mm.join(".")))
}

pub fn port_open(port: u16) -> bool {
    TcpStream::connect_timeout(&SocketAddr::from(([127, 0, 0, 1], port)), Duration::from_millis(200)).is_ok()
}

#[derive(Clone, Debug, PartialEq)]
pub enum DbStatus {
    Stopped,
    Starting,
    Running,
    /// Exited on its own; the message has the log tail.
    Failed(String),
}

#[derive(Default)]
pub struct MySql {
    child: Option<(Child, Install, u16, Instant)>,
    last_error: Option<String>,
    /// Set via `set_root_password` this session, so `stop` can still shut down cleanly.
    root_password: Option<String>,
}

impl MySql {
    pub fn status(&mut self) -> DbStatus {
        let Some((child, _, port, started)) = &mut self.child else {
            return self.last_error.clone().map_or(DbStatus::Stopped, DbStatus::Failed);
        };
        match child.try_wait() {
            Ok(None) if port_open(*port) => DbStatus::Running,
            Ok(None) if started.elapsed() < Duration::from_secs(60) => DbStatus::Starting,
            Ok(None) => DbStatus::Failed("server did not open its port within 60s; see logs\\mysql.log".into()),
            _ => {
                let code = child.try_wait().ok().flatten().map(|s| s.to_string()).unwrap_or_default();
                self.child = None;
                let msg = format!("server exited ({code}): {}", log_tail(&log_path()));
                self.last_error = Some(msg.clone());
                DbStatus::Failed(msg)
            }
        }
    }

    /// The root password set this session, if any.
    pub fn root_password(&self) -> Option<&str> {
        self.root_password.as_deref()
    }

    pub fn is_ours(&self) -> bool {
        self.child.is_some()
    }

    /// A client for the running server, usable from another thread.
    pub fn client(&self) -> Option<SqlClient> {
        self.child.as_ref().map(|(_, inst, port, _)| SqlClient {
            exe: inst.client(),
            port: *port,
            password: self.root_password.clone(),
        })
    }

    /// Start `folder`, initializing its data directory the first time.
    pub fn start(&mut self, root: &Path, folder: &str, port: u16) -> Result<String, String> {
        if self.child.is_some() {
            return Ok("already running".into());
        }
        self.last_error = None;
        if port_open(port) {
            return Err(format!(
                "port {port} is already in use: another MySQL/MariaDB (e.g. Laragon's) is running. Stop it first or change the port."
            ));
        }
        let inst = Install::new(root, folder)?;
        let mut note = String::new();
        if !inst.data_dir.join("mysql").is_dir() {
            initialize(&inst)?;
            note = format!(" Created {} (user root, no password).", inst.data_dir.display());
        }
        let my_ini = root.join("conf").join("my.ini");
        if !my_ini.exists() {
            let _ = fs::create_dir_all(my_ini.parent().unwrap_or(root));
            fs::write(&my_ini, DEFAULT_MY_INI).map_err(|e| format!("could not write {}: {e}", my_ini.display()))?;
        }
        let log = log_path();
        let _ = fs::create_dir_all(log.parent().unwrap_or(root));
        let out = fs::File::create(&log).map_err(|e| format!("could not open {}: {e}", log.display()))?;
        let err = out.try_clone().map_err(|e| e.to_string())?;

        let mut cmd = Command::new(inst.server());
        // --defaults-file must come first.
        cmd.arg(format!("--defaults-file={}", my_ini.display()))
            .arg(format!("--datadir={}", inst.data_dir.display()))
            .arg(format!("--port={port}"))
            .arg("--bind-address=127.0.0.1")
            .arg("--console")
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err);
        crate::hide_console(&mut cmd);
        let child = cmd.spawn().map_err(|e| format!("could not start {}: {e}", inst.server().display()))?;
        self.child = Some((child, inst, port, Instant::now()));
        Ok(format!("Starting {folder} on 127.0.0.1:{port}.{note}"))
    }

    /// Graceful shutdown via mysqladmin (works while root has no password),
    /// falling back to ending the process after a short wait.
    pub fn stop(&mut self) -> String {
        let Some((mut child, inst, port, _)) = self.child.take() else {
            return "not running".into();
        };
        self.last_error = None;
        let mut admin = Command::new(inst.admin());
        admin.args(["-u", "root", "--protocol=tcp", "-h", "127.0.0.1", &format!("--port={port}"), "shutdown"]).stdin(Stdio::null());
        if let Some(pw) = &self.root_password {
            admin.env("MYSQL_PWD", pw);
        }
        crate::hide_console(&mut admin);
        let graceful = admin.output().map(|o| o.status.success()).unwrap_or(false);
        let deadline = Instant::now() + Duration::from_secs(if graceful { 20 } else { 3 });
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                return "Stopped.".into();
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
        "Stopped (forced: mysqladmin shutdown was refused, e.g. root has a password).".into()
    }

    /// Set a new root password (needs the server running and root passwordless).
    pub fn set_root_password(&mut self, password: &str) -> Result<(), String> {
        let Some((_, inst, port, _)) = &self.child else {
            return Err("start MySQL / MariaDB first".into());
        };
        if !password.chars().all(|c| c.is_ascii_alphanumeric()) {
            return Err("password must be letters and digits".into());
        }
        let sql = format!("ALTER USER 'root'@'localhost' IDENTIFIED BY '{password}'; FLUSH PRIVILEGES;");
        let mut cmd = Command::new(inst.client());
        cmd.args(["-u", "root", "--protocol=tcp", "-h", "127.0.0.1", &format!("--port={port}"), "-e", &sql]).stdin(Stdio::null());
        if let Some(pw) = &self.root_password {
            cmd.env("MYSQL_PWD", pw);
        }
        crate::hide_console(&mut cmd);
        let out = cmd.output().map_err(|e| e.to_string())?;
        if out.status.success() {
            self.root_password = Some(password.to_string());
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
}

/// The mysql / mariadb command-line client pointed at our server as root.
#[derive(Clone, Debug)]
pub struct SqlClient {
    pub exe: PathBuf,
    pub port: u16,
    pub password: Option<String>,
}

impl SqlClient {
    pub fn exec(&self, sql: &str) -> Result<(), String> {
        let mut cmd = Command::new(&self.exe);
        cmd.args(["-u", "root", "--protocol=tcp", "-h", "127.0.0.1", &format!("--port={}", self.port), "-e", sql]).stdin(Stdio::null());
        if let Some(pw) = &self.password {
            cmd.env("MYSQL_PWD", pw);
        }
        let out = crate::hide_console(&mut cmd).output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }

    /// `CREATE DATABASE IF NOT EXISTS`, for names of letters, digits, `_` and `-`.
    pub fn create_database(&self, name: &str) -> Result<(), String> {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
            return Err(format!("`{name}` is not a safe database name"));
        }
        self.exec(&format!("CREATE DATABASE IF NOT EXISTS `{name}` CHARACTER SET utf8mb4 COLLATE utf8mb4_unicode_ci;"))
    }
}

impl Drop for MySql {
    fn drop(&mut self) {
        self.stop();
    }
}

fn initialize(inst: &Install) -> Result<(), String> {
    if inst.data_dir.exists() && fs::read_dir(&inst.data_dir).map(|mut d| d.next().is_some()).unwrap_or(false) {
        return Err(format!("{} exists but has no system tables; move it aside to re-create it", inst.data_dir.display()));
    }
    // The initializers create the data dir itself, but not its parent.
    if let Some(parent) = inst.data_dir.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("could not create {}: {e}", parent.display()))?;
    }
    let mut cmd = if inst.is_mysql {
        let mut c = Command::new(inst.server());
        c.arg("--initialize-insecure").arg(format!("--datadir={}", inst.data_dir.display())).arg("--console");
        c
    } else {
        let mut c = Command::new(inst.tool(&["mariadb-install-db.exe", "mysql_install_db.exe"]));
        c.arg(format!("--datadir={}", inst.data_dir.display()));
        c
    };
    cmd.stdin(Stdio::null());
    crate::hide_console(&mut cmd);
    let out = cmd.output().map_err(|e| format!("could not run the database initializer: {e}"))?;
    if !out.status.success() || !inst.data_dir.join("mysql").is_dir() {
        let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
        return Err(format!("initializing {} failed: {}", inst.data_dir.display(), last_lines(&text, 3)));
    }
    Ok(())
}

pub fn log_path() -> PathBuf {
    crate::mycraft_root().join("logs").join("mysql.log")
}

fn log_tail(path: &Path) -> String {
    last_lines(&fs::read_to_string(path).unwrap_or_default(), 3)
}

fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    lines[lines.len().saturating_sub(n)..].join(" | ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real server run: MYCRAFT_TEST_MYSQL=<folder with bin\mysql\<install>>
    /// `cargo test -- --ignored live_server --nocapture`
    #[test]
    #[ignore]
    fn live_server() {
        let root = PathBuf::from(std::env::var("MYCRAFT_TEST_MYSQL").expect("MYCRAFT_TEST_MYSQL"));
        let folder = std::env::var("MYCRAFT_TEST_FOLDER").expect("MYCRAFT_TEST_FOLDER");
        let mut db = MySql::default();
        println!("{}", db.start(&root, &folder, 3399).unwrap());
        let t = Instant::now();
        while db.status() == DbStatus::Starting && t.elapsed() < Duration::from_secs(60) {
            std::thread::sleep(Duration::from_millis(300));
        }
        assert_eq!(db.status(), DbStatus::Running, "log: {}", log_tail(&log_path()));
        println!("running after {:?}", t.elapsed());
        assert!(db.start(&root, &folder, 3399).is_ok(), "second start is a no-op");
        println!("{}", db.stop());
        assert!(!port_open(3399));
        // Restart on the now-initialized data dir.
        db.start(&root, &folder, 3399).unwrap();
        while db.status() == DbStatus::Starting {
            std::thread::sleep(Duration::from_millis(300));
        }
        assert_eq!(db.status(), DbStatus::Running);
        db.set_root_password("Abc123xyz").unwrap();
        let client = db.client().unwrap();
        client.create_database("mycraft-live").unwrap();
        client.exec("USE `mycraft-live`; CREATE TABLE t (id INT); DROP DATABASE `mycraft-live`;").unwrap();
        assert!(client.create_database("bad name").is_err());
        println!("{}", db.stop());
        assert!(!port_open(3399));
    }

    #[test]
    fn data_keys() {
        assert_eq!(data_key("mariadb-11.8.9-winx64").as_deref(), Some("mariadb-11.8"));
        assert_eq!(data_key("mariadb-10.6.21-winx64").as_deref(), Some("mariadb-10.6"));
        assert_eq!(data_key("mysql-8.4.9-winx64-WoW").as_deref(), Some("mysql-8.4"));
        assert_eq!(data_key("mysql"), None);
        assert_eq!(data_key("mariadb-latest"), None);
    }
}
