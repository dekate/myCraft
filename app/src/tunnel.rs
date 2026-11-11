use std::collections::HashMap;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq)]
pub enum TunnelStatus {
    Starting,
    Online {
        url: String,
        local_port: u16,
    },
    Error(String),
}

#[allow(dead_code)]
struct ProjectTunnel {
    pub project_name: String,
    pub local_port: u16,
    pub status: TunnelStatus,
    pub stop_flag: Arc<AtomicBool>,
    pub child: Option<Arc<Mutex<Child>>>,
}

#[derive(Clone)]
pub struct TunnelManager {
    tunnels: Arc<Mutex<HashMap<String, ProjectTunnel>>>,
    www_root: Arc<Mutex<String>>,
}

impl TunnelManager {
    pub fn new(www_root: String) -> Self {
        Self {
            tunnels: Arc::new(Mutex::new(HashMap::new())),
            www_root: Arc::new(Mutex::new(www_root)),
        }
    }

    pub fn update_www_root(&self, new_root: String) {
        if let Ok(mut r) = self.www_root.lock() {
            *r = new_root;
        }
    }

    pub fn get_status(&self, project: &str) -> Option<TunnelStatus> {
        let map = self.tunnels.lock().ok()?;
        map.get(project).map(|t| t.status.clone())
    }

    pub fn list_tunnels(&self) -> Vec<(String, TunnelStatus)> {
        if let Ok(map) = self.tunnels.lock() {
            map.iter().map(|(k, v)| (k.clone(), v.status.clone())).collect()
        } else {
            Vec::new()
        }
    }

    pub fn start_tunnel(&self, project: &str) -> Result<(), String> {
        let www_path = self.www_root.lock().map_err(|e| e.to_string())?.clone();
        let project_dir = PathBuf::from(&www_path).join(project);

        if !project_dir.is_dir() {
            return Err(format!("Project folder '{}' does not exist in www root.", project));
        }

        // Check if already active or starting
        {
            let mut map = self.tunnels.lock().map_err(|e| e.to_string())?;
            if let Some(existing) = map.get(project) {
                if existing.status == TunnelStatus::Starting || matches!(existing.status, TunnelStatus::Online { .. }) {
                    return Ok(());
                }
            }

            // Clean up any previous state
            if let Some(mut old) = map.remove(project) {
                old.stop_flag.store(true, Ordering::SeqCst);
                if let Some(c) = old.child.take() {
                    if let Ok(mut ch) = c.lock() {
                        let _ = ch.kill();
                        let _ = ch.wait();
                    }
                }
            }
        }

        // Bind dedicated ephemeral local port for this project
        let listener = TcpListener::bind("127.0.0.1:0")
            .map_err(|e| format!("Failed to bind local port: {}", e))?;
        let local_port = listener
            .local_addr()
            .map_err(|e| format!("Failed to read local port: {}", e))?
            .port();
        let _ = listener.set_nonblocking(true);

        let stop_flag = Arc::new(AtomicBool::new(false));

        // Insert initial starting status
        {
            let mut map = self.tunnels.lock().map_err(|e| e.to_string())?;
            map.insert(
                project.to_string(),
                ProjectTunnel {
                    project_name: project.to_string(),
                    local_port,
                    status: TunnelStatus::Starting,
                    stop_flag: stop_flag.clone(),
                    child: None,
                },
            );
        }

        // Spawn dedicated HTTP listener for this project
        let stop_flag_listener = stop_flag.clone();
        let project_dir_clone = project_dir.clone();
        let project_name_clone = project.to_string();
        thread::spawn(move || {
            while !stop_flag_listener.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let p = project_dir_clone.clone();
                        let name = project_name_clone.clone();
                        thread::spawn(move || {
                            handle_project_client(stream, &p, &name);
                        });
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(50));
                    }
                    Err(_) => {
                        thread::sleep(Duration::from_millis(50));
                    }
                }
            }
        });

        // Spawn Pinggy SSH process: ssh -p 443 -o StrictHostKeyChecking=no -o ServerAliveInterval=30 -R0:localhost:{port} a.pinggy.io
        let mut cmd = get_ssh_command();
        cmd.args([
            "-T",
            "-p",
            "443",
            "-o",
            "StrictHostKeyChecking=no",
            "-o",
            "ServerAliveInterval=30",
            "-R",
            &format!("0:localhost:{}", local_port),
            "a.pinggy.io",
        ]);
        // stdin must be NUL: with an inherited stdin, Pinggy connects but never
        // prints the public URLs, and the tunnel sits at "Connecting" forever.
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        crate::hide_console(&mut cmd);

        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                stop_flag.store(true, Ordering::SeqCst);
                let err_msg = format!("Failed to launch OpenSSH: {}. Make sure 'ssh' is installed.", e);
                if let Ok(mut map) = self.tunnels.lock() {
                    if let Some(t) = map.get_mut(project) {
                        t.status = TunnelStatus::Error(err_msg.clone());
                    }
                }
                return Err(err_msg);
            }
        };

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let child_arc = Arc::new(Mutex::new(child));

        // Store child handle
        {
            if let Ok(mut map) = self.tunnels.lock() {
                if let Some(t) = map.get_mut(project) {
                    t.child = Some(child_arc.clone());
                }
            }
        }

        // Background reader for stdout to capture Pinggy public URL
        let tunnels_clone = self.tunnels.clone();
        let proj_name = project.to_string();
        let stop_flag_reader = stop_flag.clone();

        thread::spawn(move || {
            if let Some(out) = stdout {
                let reader = BufReader::new(out);
                for line in reader.lines() {
                    if stop_flag_reader.load(Ordering::SeqCst) {
                        break;
                    }
                    if let Ok(l) = line {
                        if let Some(url) = extract_pinggy_url(&l) {
                            if let Ok(mut map) = tunnels_clone.lock() {
                                if let Some(t) = map.get_mut(&proj_name) {
                                    t.status = TunnelStatus::Online {
                                        url,
                                        local_port,
                                    };
                                }
                            }
                        }
                    }
                }
            }
        });

        // Give up if no public URL shows up: Pinggy normally answers in ~2s.
        let tunnels_timeout = self.tunnels.clone();
        let proj_name_timeout = project.to_string();
        let child_timeout = child_arc.clone();
        thread::spawn(move || {
            thread::sleep(Duration::from_secs(30));
            let timed_out = match tunnels_timeout.lock() {
                Ok(mut map) => match map.get_mut(&proj_name_timeout) {
                    Some(t)
                        if t.status == TunnelStatus::Starting
                            && t.child.as_ref().is_some_and(|c| Arc::ptr_eq(c, &child_timeout)) =>
                    {
                        t.stop_flag.store(true, Ordering::SeqCst);
                        t.status = TunnelStatus::Error("Pinggy did not return a public URL within 30s.".into());
                        true
                    }
                    _ => false,
                },
                Err(_) => false,
            };
            // Kill after releasing the map lock: the exit supervisor locks child, then map.
            if timed_out {
                if let Ok(mut c) = child_timeout.lock() {
                    let _ = c.kill();
                }
            }
        });

        // Background supervisor for stderr and process exit
        let tunnels_err = self.tunnels.clone();
        let proj_name_err = project.to_string();
        let stop_flag_err = stop_flag;
        let child_for_wait = child_arc;

        thread::spawn(move || {
            let mut err_output = String::new();
            if let Some(err) = stderr {
                let reader = BufReader::new(err);
                for line in reader.lines().flatten() {
                    if !err_output.is_empty() {
                        err_output.push(' ');
                    }
                    err_output.push_str(&line);
                }
            }

            // Wait for child process to exit
            let exit_status = {
                if let Ok(mut c) = child_for_wait.lock() {
                    c.wait().ok()
                } else {
                    None
                }
            };

            // If it exited while not intentionally stopped, mark error
            if !stop_flag_err.load(Ordering::SeqCst) {
                stop_flag_err.store(true, Ordering::SeqCst);
                if let Ok(mut map) = tunnels_err.lock() {
                    if let Some(t) = map.get_mut(&proj_name_err) {
                        let msg = if !err_output.trim().is_empty() {
                            format!("SSH exited: {}", err_output.trim())
                        } else if let Some(st) = exit_status {
                            format!("SSH process terminated ({})", st)
                        } else {
                            "SSH tunnel closed unexpectedly".to_string()
                        };
                        t.status = TunnelStatus::Error(msg);
                    }
                }
            }
        });

        Ok(())
    }

    pub fn stop_tunnel(&self, project: &str) {
        if let Ok(mut map) = self.tunnels.lock() {
            if let Some(mut t) = map.remove(project) {
                t.stop_flag.store(true, Ordering::SeqCst);
                if let Some(child_arc) = t.child.take() {
                    if let Ok(mut child) = child_arc.lock() {
                        let pid = child.id();
                        let _ = child.kill();
                        let _ = child.wait();
                        #[cfg(windows)]
                        {
                            let _ = crate::hide_console(
                                Command::new("taskkill").args(["/F", "/T", "/PID", &pid.to_string()]),
                            )
                            .status();
                        }
                    }
                }
            }
        }
    }

    pub fn stop_all(&self) {
        let keys: Vec<String> = if let Ok(map) = self.tunnels.lock() {
            map.keys().cloned().collect()
        } else {
            Vec::new()
        };
        for k in keys {
            self.stop_tunnel(&k);
        }
    }
}

impl Drop for TunnelManager {
    fn drop(&mut self) {
        if Arc::strong_count(&self.tunnels) <= 1 {
            self.stop_all();
        }
    }
}

/// Helper to get cross-platform ssh command
pub fn get_ssh_command() -> Command {
    #[cfg(windows)]
    {
        let sys_ssh = Path::new("C:\\Windows\\System32\\OpenSSH\\ssh.exe");
        if sys_ssh.exists() {
            Command::new(sys_ssh)
        } else {
            Command::new("ssh")
        }
    }
    #[cfg(not(windows))]
    {
        Command::new("ssh")
    }
}

/// Extract Pinggy public URL from command output lines
pub fn extract_pinggy_url(line: &str) -> Option<String> {
    for word in line.split_whitespace() {
        let clean = word.trim_matches(|c: char| {
            !c.is_alphanumeric() && c != ':' && c != '/' && c != '.' && c != '-'
        });
        if (clean.starts_with("https://") || clean.starts_with("http://"))
            && clean.contains("pinggy")
            && !clean.contains("dashboard.pinggy.io")
            && clean != "https://pinggy.io"
            && clean != "http://pinggy.io"
        {
            return Some(clean.to_string());
        }
    }
    None
}

fn handle_project_client(mut stream: TcpStream, project_dir: &Path, project_name: &str) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));

    let mut buffer = [0u8; 8192];
    let bytes_read = match stream.read(&mut buffer) {
        Ok(n) if n > 0 => n,
        _ => return,
    };

    let request_str = String::from_utf8_lossy(&buffer[..bytes_read]);
    serve_project(&mut stream, project_dir, project_name, &request_str, &buffer[..bytes_read]);
}

/// First index file in `dir`, in web-server order (html, htm, php).
fn find_index(dir: &Path) -> Option<PathBuf> {
    ["index.html", "index.htm", "index.php"]
        .iter()
        .map(|f| dir.join(f))
        .find(|p| p.is_file())
}

/// Web root of a project: its `public/` folder when present (Laravel, Symfony,
/// Slim…), as Laragon's auto virtual hosts do; otherwise the folder itself.
/// In Node projects (Vite, Next, CRA) `public/` only holds static assets, so
/// there it counts only when it has a PHP front controller.
pub fn document_root(project_dir: &Path) -> PathBuf {
    let public = project_dir.join("public");
    let is_node = project_dir.join("package.json").is_file();
    if public.is_dir() && (!is_node || public.join("index.php").is_file()) {
        public
    } else {
        project_dir.to_path_buf()
    }
}

/// Serve one already-read request from `project_dir` (static files, PHP via
/// php-cgi, directory listing). Shared by the Pinggy tunnel and `*.go` vhosts.
pub fn serve_project(
    stream: &mut dyn crate::server::Conn,
    project_dir: &Path,
    project_name: &str,
    request_str: &str,
    raw_request: &[u8],
) {
    let first_line = request_str.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();

    if parts.len() < 2 {
        let resp = "HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    let raw_url = parts[1];
    let path_str = match raw_url.find('?') {
        Some(idx) => &raw_url[..idx],
        None => raw_url,
    };

    let decoded_path = crate::server::percent_decode(path_str);
    let clean_path = decoded_path.trim_start_matches(['/', '\\']);

    // Security check: prevent directory traversal (checked after decoding so
    // %2e%2e can't slip past) and absolute paths like C:\ that replace the root.
    if clean_path.contains("..") || clean_path.contains(':') {
        let resp = "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    let doc_root = document_root(project_dir);
    let requested = doc_root.join(clean_path);

    // nginx: `try_files $uri $uri/ /index.php?$query_string` — the front
    // controller catches every path that isn't a real file or indexed folder.
    let target_file = if requested.is_file() {
        Some(requested.clone())
    } else if requested.is_dir() {
        find_index(&requested)
    } else {
        None
    }
    .or_else(|| Some(doc_root.join("index.php")).filter(|p| p.is_file()));

    let target_file = match target_file {
        Some(f) => f,
        None if requested.is_dir() => {
            serve_directory_splash(stream, &requested, project_name);
            return;
        }
        None => PathBuf::new(),
    };

    if target_file.is_file() {
        if target_file.extension().and_then(|s| s.to_str()) == Some("php") {
            let served = find_php_cgi(project_dir).and_then(|cgi| {
                try_serve_php_cgi(stream, &cgi, &target_file, request_str, raw_request, &doc_root)
                    .then_some(())
                    .ok_or_else(|| format!("{} failed to run.", cgi.display()))
            });
            if let Err(why) = served {
                // Never fall back to sending PHP source as a static file.
                let body = format!("502 Bad Gateway: {why}");
                let resp = format!(
                    "HTTP/1.1 502 Bad Gateway\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(resp.as_bytes());
            }
            return;
        }

        if let Ok(contents) = fs::read(&target_file) {
            let mime = get_mime_type(&target_file);
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                mime,
                contents.len()
            );
            let _ = stream.write_all(headers.as_bytes());
            let _ = stream.write_all(&contents);
            return;
        }
    }

    // 404 Not Found
    let body = format!(
        "<!DOCTYPE html><html><body><h1>404 Not Found</h1><p>Resource {} not found in project {}.</p></body></html>",
        clean_path, project_name
    );
    let resp = format!(
        "HTTP/1.1 404 Not Found\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(resp.as_bytes());
}

fn serve_directory_splash(stream: &mut dyn crate::server::Conn, dir: &Path, project_name: &str) {
    let mut files_html = String::new();
    if let Ok(entries) = fs::read_dir(dir) {
        let mut names: Vec<String> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with('.') && !name.starts_with('_') {
                names.push(name);
            }
        }
        names.sort();
        for name in names {
            let is_dir = dir.join(&name).is_dir();
            let icon = if is_dir { "📁" } else { "📄" };
            let slash = if is_dir { "/" } else { "" };
            files_html.push_str(&format!(
                r#"<li style="margin: 0.35rem 0;"><a href="{name}{slash}" style="color: #38bdf8; text-decoration: none;">{icon} {name}{slash}</a></li>"#
            ));
        }
    }
    if files_html.is_empty() {
        files_html = "<li style='color: #94a3b8;'>No public files found in this directory.</li>".to_string();
    }

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>{name} &bull; myCraft</title>
    <style>
        body {{ background: #0b1120; color: #f1f5f9; font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif; padding: 2rem; max-width: 680px; margin: 0 auto; line-height: 1.5; }}
        .header {{ display: flex; align-items: center; justify-content: space-between; border-bottom: 1px solid #1e293b; padding-bottom: 1rem; margin-bottom: 1.5rem; }}
        .badge {{ background: rgba(16, 185, 129, 0.15); color: #10b981; border: 1px solid rgba(16, 185, 129, 0.3); padding: 0.25rem 0.65rem; border-radius: 9999px; font-size: 0.8rem; font-weight: 600; }}
        h1 {{ margin: 0; font-size: 1.35rem; color: #f8fafc; }}
        ul {{ list-style: none; padding: 0; margin: 0; }}
        .card {{ background: #131c2e; border: 1px solid #1e293b; border-radius: 8px; padding: 1.25rem; }}
        .hint {{ margin-top: 1rem; font-size: 0.85rem; color: #64748b; text-align: center; }}
    </style>
</head>
<body>
    <div class="header">
        <h1>📁 {name}</h1>
        <span class="badge">🟢 Live</span>
    </div>
    <div class="card">
        <h3 style="margin-top: 0; color: #cbd5e1; font-size: 0.95rem; margin-bottom: 0.75rem;">Project Files</h3>
        <ul>{files}</ul>
    </div>
    <div class="hint">No index file here &mdash; served from your local myCraft dev box</div>
</body>
</html>"#,
        name = project_name,
        files = files_html
    );

    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        html.len(),
        html
    );
    let _ = stream.write_all(resp.as_bytes());
}

fn try_serve_php_cgi(
    stream: &mut dyn crate::server::Conn,
    php_cgi: &Path,
    php_file: &Path,
    request_str: &str,
    raw_buffer: &[u8],
    doc_root: &Path,
) -> bool {
    let first_line = request_str.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        return false;
    }
    let method = parts[0];
    let raw_url = parts[1];
    let query_part = raw_url.split_once('?').map(|(_, q)| q).unwrap_or("");

    // URL path of the script itself (e.g. `/index.php` for a front-controller
    // request to `/login`); frameworks derive their base URL from it.
    let script_name = format!(
        "/{}",
        php_file
            .strip_prefix(doc_root)
            .unwrap_or(php_file)
            .to_string_lossy()
            .replace('\\', "/")
    );

    // Request headers become HTTP_* variables (Host, Cookie, X-CSRF-TOKEN…).
    let headers: Vec<(&str, &str)> = request_str
        .lines()
        .skip(1)
        .take_while(|l| !l.trim().is_empty())
        .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim(), v.trim())))
        .collect();
    let header = |name: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| *v);

    let mut body_bytes = Vec::new();
    if let Some(pos) = raw_buffer.windows(4).position(|w| w == b"\r\n\r\n") {
        body_bytes.extend_from_slice(&raw_buffer[pos + 4..]);
    }
    // The caller read only the first chunk; pull the rest of a larger body.
    let content_length: usize = header("Content-Length").and_then(|v| v.parse().ok()).unwrap_or(0);
    if content_length > body_bytes.len() {
        let missing = (content_length - body_bytes.len()) as u64;
        let _ = (&mut *stream).take(missing).read_to_end(&mut body_bytes);
    }

    let host = header("Host").unwrap_or("localhost");
    let (server_name, server_port) = match host.rsplit_once(':') {
        Some((h, p)) => (h, p),
        None => (host, "80"),
    };

    let mut cmd = Command::new(&php_cgi);
    for (k, v) in &headers {
        let key = k.to_ascii_uppercase().replace('-', "_");
        if key != "CONTENT_TYPE" && key != "CONTENT_LENGTH" && key != "PROXY" {
            cmd.env(format!("HTTP_{key}"), v);
        }
    }
    if let Some(ct) = header("Content-Type") {
        cmd.env("CONTENT_TYPE", ct);
    }
    if stream.is_tls() {
        cmd.env("HTTPS", "on");
    }
    if let Some(peer) = stream.peer() {
        cmd.env("REMOTE_ADDR", peer.ip().to_string())
            .env("REMOTE_PORT", peer.port().to_string());
    }
    cmd.env("GATEWAY_INTERFACE", "CGI/1.1")
        .env("SERVER_SOFTWARE", "myCraft")
        .env("SERVER_PROTOCOL", "HTTP/1.1")
        .env("SERVER_NAME", server_name)
        .env("SERVER_PORT", server_port)
        .env("REQUEST_METHOD", method)
        .env("REQUEST_URI", raw_url)
        .env("SCRIPT_FILENAME", php_file)
        .env("SCRIPT_NAME", &script_name)
        .env("PHP_SELF", &script_name)
        .env("QUERY_STRING", query_part)
        .env("DOCUMENT_ROOT", doc_root)
        .env("CONTENT_LENGTH", body_bytes.len().to_string())
        .env("REDIRECT_STATUS", "200")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    crate::hide_console(&mut cmd);

    if let Ok(mut child) = cmd.spawn() {
        if !body_bytes.is_empty() {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(&body_bytes);
            }
        }
        if let Ok(out) = child.wait_with_output() {
            let errors = String::from_utf8_lossy(&out.stderr);
            for line in errors.lines().map(str::trim).filter(|l| !l.is_empty()) {
                crate::server::append_log("php_error.log", &format!("{} {} {line}", crate::state::MyCraftState::current_time_str(), php_file.display()));
            }
            if out.status.success() || !out.stdout.is_empty() {
                let stdout_data = out.stdout;
                let sep = b"\r\n\r\n";
                let sep_lf = b"\n\n";

                let (cgi_headers, cgi_body) = if let Some(p) = stdout_data.windows(4).position(|w| w == sep) {
                    (&stdout_data[..p], &stdout_data[p + 4..])
                } else if let Some(p) = stdout_data.windows(2).position(|w| w == sep_lf) {
                    (&stdout_data[..p], &stdout_data[p + 2..])
                } else {
                    (&b""[..], &stdout_data[..])
                };

                let headers_str = String::from_utf8_lossy(cgi_headers);
                let mut status_line = "HTTP/1.1 200 OK".to_string();
                let mut response_headers = String::new();
                for line in headers_str.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    let lower = line.to_ascii_lowercase();
                    if let Some(val) = lower.strip_prefix("status:").map(|_| line[7..].trim()) {
                        status_line = format!("HTTP/1.1 {}", val);
                    } else if !lower.starts_with("content-length:") && !lower.starts_with("connection:") {
                        response_headers.push_str(line);
                        response_headers.push_str("\r\n");
                    }
                }
                let response_headers = format!(
                    "{}\r\n{}Content-Length: {}\r\nConnection: close\r\n\r\n",
                    status_line,
                    response_headers,
                    cgi_body.len()
                );
                let _ = stream.write_all(response_headers.as_bytes());
                let _ = stream.write_all(cgi_body);
                return true;
            }
        }
    }

    false
}

/// php-cgi for this project: its pinned / composer-required PHP under bin\php
/// (see `php_update::for_project`), or the system php-cgi off Windows.
fn find_php_cgi(project_dir: &Path) -> Result<PathBuf, String> {
    #[cfg(windows)]
    {
        let php_bin_dir = crate::mycraft_root().join("bin").join("php");
        crate::php_update::for_project(&php_bin_dir, project_dir).map(|b| php_bin_dir.join(b.dir_name).join("php-cgi.exe"))
    }
    #[cfg(not(windows))]
    {
        let _ = project_dir;
        Command::new("which")
            .arg("php-cgi")
            .output()
            .ok()
            .filter(|p| p.status.success())
            .map(|p| PathBuf::from(String::from_utf8_lossy(&p.stdout).trim()))
            .filter(|p| !p.as_os_str().is_empty())
            .ok_or_else(|| "php-cgi not found on PATH.".to_string())
    }
}

fn get_mime_type(path: &Path) -> &'static str {
    match path.extension().and_then(|s| s.to_str()).unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "webp" => "image/webp",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "pdf" => "application/pdf",
        "txt" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}
