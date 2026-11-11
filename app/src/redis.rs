//! Runs Redis from `bin\redis\<install>` with `conf\redis.conf` (written on
//! first start), data in `data\redis`, output in `logs\redis.log`.

use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::mysql::{port_open, DbStatus};

pub const DEFAULT_CONF: &str = "\
# myCraft Redis settings. port, bind and dir are set by myCraft when it starts
# the server, so they are not listed here.
databases 16
save 900 1
save 300 10
maxmemory 256mb
maxmemory-policy allkeys-lru
";

pub fn conf_path() -> PathBuf {
    crate::mycraft_root().join("conf").join("redis.conf")
}

pub fn log_path() -> PathBuf {
    crate::mycraft_root().join("logs").join("redis.log")
}

/// `name.exe` in the install folder or one level below it.
pub fn tool(folder: &str, name: &str) -> Option<PathBuf> {
    let dir = crate::packages::bin_dir("redis").join(folder);
    std::iter::once(dir.join(name))
        .chain(fs::read_dir(&dir).into_iter().flatten().flatten().map(|e| e.path().join(name)))
        .find(|p| p.is_file())
}

#[derive(Default)]
pub struct Redis {
    child: Option<(Child, String, u16, Instant)>,
    last_error: Option<String>,
}

impl Redis {
    pub fn status(&mut self) -> DbStatus {
        let Some((child, _, port, started)) = &mut self.child else {
            return self.last_error.clone().map_or(DbStatus::Stopped, DbStatus::Failed);
        };
        match child.try_wait() {
            Ok(None) if port_open(*port) => DbStatus::Running,
            Ok(None) if started.elapsed() < Duration::from_secs(20) => DbStatus::Starting,
            Ok(None) => DbStatus::Failed("server did not open its port; see logs\\redis.log".into()),
            _ => {
                self.child = None;
                let log = fs::read_to_string(log_path()).unwrap_or_default();
                let tail: Vec<&str> = log.lines().filter(|l| !l.trim().is_empty()).collect();
                let msg = format!("server exited: {}", tail[tail.len().saturating_sub(2)..].join(" | "));
                self.last_error = Some(msg.clone());
                DbStatus::Failed(msg)
            }
        }
    }

    pub fn is_ours(&self) -> bool {
        self.child.is_some()
    }

    pub fn start(&mut self, folder: &str, port: u16) -> Result<String, String> {
        if self.child.is_some() {
            return Ok("already running".into());
        }
        self.last_error = None;
        if port_open(port) {
            return Err(format!("port {port} is already in use by another program."));
        }
        let server = tool(folder, "redis-server.exe").ok_or_else(|| format!("bin\\redis\\{folder} has no redis-server.exe"))?;
        let root = crate::mycraft_root();
        let conf = conf_path();
        if !conf.exists() {
            let _ = fs::create_dir_all(conf.parent().unwrap_or(root));
            fs::write(&conf, DEFAULT_CONF).map_err(|e| format!("could not write {}: {e}", conf.display()))?;
        }
        let data = root.join("data").join("redis");
        fs::create_dir_all(&data).map_err(|e| format!("could not create {}: {e}", data.display()))?;
        let log = log_path();
        let _ = fs::create_dir_all(log.parent().unwrap_or(root));
        let out = fs::File::create(&log).map_err(|e| format!("could not open {}: {e}", log.display()))?;
        let err = out.try_clone().map_err(|e| e.to_string())?;

        // Run from the data dir with relative paths: the Windows ports mangle
        // drive-letter paths in --dir.
        let mut cmd = Command::new(&server);
        cmd.current_dir(&data)
            .arg(&conf)
            .args(["--port", &port.to_string(), "--bind", "127.0.0.1", "--dir", "."])
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(err);
        crate::hide_console(&mut cmd);
        let child = cmd.spawn().map_err(|e| format!("could not start {}: {e}", server.display()))?;
        self.child = Some((child, folder.to_string(), port, Instant::now()));
        Ok(format!("Starting Redis {folder} on 127.0.0.1:{port}."))
    }

    pub fn stop(&mut self) -> String {
        let Some((mut child, folder, port, _)) = self.child.take() else {
            return "not running".into();
        };
        self.last_error = None;
        let _ = cli(&folder, port, &["shutdown"]);
        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = child.try_wait() {
                return "Stopped.".into();
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        let _ = child.kill();
        let _ = child.wait();
        "Stopped (forced).".into()
    }

    /// The running install and port, for redis-cli.
    pub fn target(&self) -> Option<(String, u16)> {
        self.child.as_ref().map(|(_, f, p, _)| (f.clone(), *p))
    }
}

impl Drop for Redis {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Run one redis-cli command against the server; returns its output.
pub fn cli(folder: &str, port: u16, args: &[&str]) -> Result<String, String> {
    let exe = tool(folder, "redis-cli.exe").ok_or("redis-cli.exe not found")?;
    let mut cmd = Command::new(exe);
    cmd.args(["-h", "127.0.0.1", "-p", &port.to_string()]).args(args).stdin(Stdio::null());
    let out = crate::hide_console(&mut cmd).output().map_err(|e| e.to_string())?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr)).trim().to_string();
    if out.status.success() && !text.starts_with("ERR") && !text.contains("Could not connect") {
        Ok(text)
    } else {
        Err(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real run of the newest bin\redis install on port 6399:
    /// `MYCRAFT_TEST_REDIS=<folder with redis-server.exe> cargo test -- --ignored live_redis --nocapture`
    #[test]
    #[ignore]
    fn live_redis() {
        // An absolute path works too: bin_dir().join(abs) is abs.
        let folder = std::env::var("MYCRAFT_TEST_REDIS").ok().or_else(|| crate::packages::installed("redis").into_iter().next()).expect("set MYCRAFT_TEST_REDIS to a Redis folder");
        let mut r = Redis::default();
        println!("{}", r.start(&folder, 6399).unwrap());
        let deadline = Instant::now() + Duration::from_secs(10);
        while r.status() != DbStatus::Running {
            assert!(Instant::now() < deadline, "{:?}", r.status());
            std::thread::sleep(Duration::from_millis(100));
        }
        assert_eq!(cli(&folder, 6399, &["SET", "k", "v"]).unwrap(), "OK");
        assert_eq!(cli(&folder, 6399, &["FLUSHALL"]).unwrap(), "OK");
        assert_eq!(cli(&folder, 6399, &["DBSIZE"]).unwrap(), "0");
        println!("{}", r.stop());
        assert!(!port_open(6399));
    }
}
