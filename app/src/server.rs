use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct CapturedEmail {
    pub id: usize,
    pub timestamp: String,
    pub from: String,
    pub to: String,
    pub subject: String,
    pub body: String,
}

static MAIL_INBOX: Mutex<Vec<CapturedEmail>> = Mutex::new(Vec::new());

/// Mail catcher: web UI on `mailpit_port`, SMTP sink on `smtp_port` (both
/// from settings, re-read each loop so a saved port applies immediately).
pub fn start_mailpit_server(running_flag: Arc<AtomicBool>) {
    let flag1 = running_flag.clone();
    thread::spawn(move || listen(flag1, || Some(crate::settings::current().mailpit_port), handle_mailpit_web_client));
    thread::spawn(move || {
        listen(running_flag, || Some(crate::settings::current().smtp_port), |s| {
            thread::spawn(move || handle_mailpit_smtp_client(s));
        })
    });
}

/// Accept connections on `port_of()` while `flag` is on. The listener is
/// dropped when the flag goes off (freeing the port) and re-bound when the
/// port changes.
fn listen(flag: Arc<AtomicBool>, port_of: impl Fn() -> Option<u16>, mut on_conn: impl FnMut(TcpStream)) {
    let mut bound: Option<(u16, TcpListener)> = None;
    loop {
        let want = if flag.load(Ordering::SeqCst) { port_of() } else { None };
        if bound.as_ref().map(|b| b.0) != want {
            bound = want.and_then(|p| {
                TcpListener::bind(("127.0.0.1", p)).ok().map(|l| {
                    let _ = l.set_nonblocking(true);
                    (p, l)
                })
            });
            if want.is_some() && bound.is_none() {
                thread::sleep(Duration::from_secs(1));
                continue;
            }
        }
        match &bound {
            Some((_, l)) => match l.accept() {
                Ok((s, _)) => {
                    let _ = s.set_nonblocking(false);
                    on_conn(s);
                }
                Err(_) => thread::sleep(Duration::from_millis(30)),
            },
            None => thread::sleep(Duration::from_millis(200)),
        }
    }
}

fn handle_mailpit_web_client(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

    let mut buffer = [0u8; 4096];
    let bytes_read = match stream.read(&mut buffer) {
        Ok(n) if n > 0 => n,
        _ => return,
    };

    let request_str = String::from_utf8_lossy(&buffer[..bytes_read]);
    let first_line = request_str.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();

    if parts.len() < 2 || parts[0] != "GET" {
        let resp = "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n";
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    let mut path = parts[1];
    if let Some(idx) = path.find('?') {
        path = &path[..idx];
    }

    if path == "/test-email" {
        let mut inbox = MAIL_INBOX.lock().unwrap();
        let id = inbox.len() + 1;
        inbox.push(CapturedEmail {
            id,
            timestamp: crate::state::MyCraftState::current_time_str(),
            from: "app@mycraft.local".to_string(),
            to: "developer@example.go".to_string(),
            subject: format!("Test Email #{} from myCraft", id),
            body: format!("This test email was added from the web inbox. Apps deliver to SMTP 127.0.0.1:{}.", crate::settings::current().smtp_port),
        });
        let resp = "HTTP/1.1 302 Found\r\nLocation: /\r\nContent-Length: 0\r\n\r\n";
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    if path == "/purge" {
        let mut inbox = MAIL_INBOX.lock().unwrap();
        inbox.clear();
        let resp = "HTTP/1.1 302 Found\r\nLocation: /\r\nContent-Length: 0\r\n\r\n";
        let _ = stream.write_all(resp.as_bytes());
        return;
    }

    let inbox = MAIL_INBOX.lock().unwrap();
    let count = inbox.len();

    let cfg = crate::settings::current();
    let (smtp, web_port) = (cfg.smtp_port, cfg.web_port);
    let mut rows = String::new();
    if count == 0 {
        rows.push_str(&format!(r#"<tr><td colspan="5" style="text-align:center; padding: 2.5rem; color: #94a3b8;">
            No captured messages yet.<br>
            <span style="font-size:0.85rem;">Send emails to <strong>127.0.0.1:{smtp}</strong> via PHP mail(), Laravel, or Node.js nodemailer, or click <strong>Send Test Email</strong> above.</span>
        </td></tr>"#));
    } else {
        for email in inbox.iter().rev() {
            rows.push_str(&format!(
                r#"<tr style="border-bottom: 1px solid #334155;">
                    <td style="padding: 0.75rem 1rem; color: #94a3b8; font-size:0.85rem;">#{} • {}</td>
                    <td style="padding: 0.75rem 1rem; font-weight:600; color: #f8fafc;">{}</td>
                    <td style="padding: 0.75rem 1rem; color: #cbd5e1;">{}</td>
                    <td style="padding: 0.75rem 1rem; color: #38bdf8; font-weight:600;">{}</td>
                    <td style="padding: 0.75rem 1rem; color: #94a3b8; font-size:0.85rem; max-width:260px; overflow:hidden; text-overflow:ellipsis; white-space:nowrap;">{}</td>
                </tr>"#,
                email.id,
                html_escape(&email.timestamp),
                html_escape(&email.from),
                html_escape(&email.to),
                html_escape(&email.subject),
                html_escape(&email.body.replace(['\n', '\r'], " "))
            ));
        }
    }

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Mailpit - Local Mail Catcher</title>
    <style>
        body {{
            background: #0f172a;
            color: #f8fafc;
            font-family: system-ui, -apple-system, sans-serif;
            margin: 0;
            padding: 0;
        }}
        .header {{
            background: #1e293b;
            border-bottom: 1px solid #334155;
            padding: 1.25rem 2rem;
            display: flex;
            justify-content: space-between;
            align-items: center;
        }}
        .badge {{
            background: rgba(16, 185, 129, 0.2);
            color: #10b981;
            border: 1px solid rgba(16, 185, 129, 0.4);
            padding: 0.25rem 0.6rem;
            border-radius: 9999px;
            font-size: 0.8rem;
            font-weight: 600;
        }}
        .container {{
            max-width: 1000px;
            margin: 2rem auto;
            padding: 0 1.5rem;
        }}
        .btn {{
            background: #2563eb;
            color: white;
            text-decoration: none;
            padding: 0.45rem 0.9rem;
            border-radius: 0.375rem;
            font-size: 0.85rem;
            font-weight: 600;
            display: inline-block;
            border: none;
            cursor: pointer;
        }}
        .btn:hover {{ background: #1d4ed8; }}
        .btn-danger {{ background: #dc2626; }}
        .btn-danger:hover {{ background: #b91c1c; }}
        .btn-secondary {{ background: #334155; }}
        .btn-secondary:hover {{ background: #475569; }}
        table {{
            width: 100%;
            border-collapse: collapse;
            background: #1e293b;
            border-radius: 0.5rem;
            overflow: hidden;
            border: 1px solid #334155;
            margin-top: 1rem;
        }}
        th {{
            background: #0f172a;
            text-align: left;
            padding: 0.75rem 1rem;
            font-size: 0.85rem;
            color: #94a3b8;
            border-bottom: 1px solid #334155;
        }}
    </style>
</head>
<body>
    <div class="header">
        <div style="display:flex; align-items:center; gap: 0.75rem;">
            <span style="font-size: 1.5rem;">📬</span>
            <span style="font-weight: 800; font-size: 1.25rem;">myCraft Mail Catcher</span>
            <span class="badge">SMTP 127.0.0.1:{smtp}</span>
        </div>
        <div style="display:flex; gap: 0.5rem;">
            <a href="/test-email" class="btn">⚡ Send Test Email</a>
            <a href="/purge" class="btn btn-danger">🗑 Purge All</a>
            <a href="http://localhost:{web_port}" class="btn btn-secondary">← Back to myCraft</a>
        </div>
    </div>
    <div class="container">
        <div style="display:flex; justify-content:space-between; align-items:center;">
            <h2>Captured Messages ({count})</h2>
            <span style="color:#94a3b8; font-size:0.85rem;">Listening on SMTP port {smtp}</span>
        </div>
        <table>
            <thead>
                <tr>
                    <th>Time</th>
                    <th>From</th>
                    <th>To</th>
                    <th>Subject</th>
                    <th>Preview</th>
                </tr>
            </thead>
            <tbody>
                {rows}
            </tbody>
        </table>
    </div>
</body>
</html>"#,
        count = count,
        rows = rows
    );

    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        html.len(),
        html
    );
    let _ = stream.write_all(response.as_bytes());
}

fn handle_mailpit_smtp_client(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    if stream.write_all(b"220 mailpit.local ESMTP Mailpit Ready\r\n").is_err() {
        return;
    }

    let mut from_addr = String::new();
    let mut to_addr = String::new();
    let mut subject = String::new();
    let mut data_mode = false;
    let mut body_acc = String::new();

    let stream_clone = match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    };
    let mut reader = std::io::BufReader::new(stream_clone);
    use std::io::BufRead;

    let mut line = String::new();
    while let Ok(n) = reader.read_line(&mut line) {
        if n == 0 {
            break;
        }
        let trimmed = line.trim_end();

        if data_mode {
            if trimmed == "." {
                let mut inbox = MAIL_INBOX.lock().unwrap();
                let id = inbox.len() + 1;
                inbox.push(CapturedEmail {
                    id,
                    timestamp: crate::state::MyCraftState::current_time_str(),
                    from: if from_addr.is_empty() { "unknown@local".into() } else { from_addr.clone() },
                    to: if to_addr.is_empty() { "recipient@local".into() } else { to_addr.clone() },
                    subject: if subject.is_empty() { "(No Subject)".into() } else { subject.clone() },
                    body: body_acc.clone(),
                });
                let _ = stream.write_all(b"250 2.0.0 Ok: queued\r\n");
                data_mode = false;
                body_acc.clear();
            } else {
                if trimmed.to_lowercase().starts_with("subject:") && subject.is_empty() {
                    subject = trimmed[8..].trim().to_string();
                }
                body_acc.push_str(trimmed);
                body_acc.push('\n');
            }
        } else {
            let upper = trimmed.to_uppercase();
            if upper.starts_with("HELO") || upper.starts_with("EHLO") {
                let _ = stream.write_all(b"250-mailpit.local\r\n250 8BITMIME\r\n");
            } else if upper.starts_with("MAIL FROM:") {
                from_addr = trimmed[10..].trim().trim_matches(|c| c == '<' || c == '>').to_string();
                let _ = stream.write_all(b"250 2.1.0 Ok\r\n");
            } else if upper.starts_with("RCPT TO:") {
                to_addr = trimmed[8..].trim().trim_matches(|c| c == '<' || c == '>').to_string();
                let _ = stream.write_all(b"250 2.1.5 Ok\r\n");
            } else if upper == "DATA" {
                data_mode = true;
                let _ = stream.write_all(b"354 End data with <CR><LF>.<CR><LF>\r\n");
            } else if upper == "QUIT" {
                let _ = stream.write_all(b"221 2.0.0 Bye\r\n");
                break;
            } else if upper == "RSET" || upper == "NOOP" {
                let _ = stream.write_all(b"250 2.0.0 Ok\r\n");
            } else {
                let _ = stream.write_all(b"502 Command not implemented\r\n");
            }
        }
        line.clear();
    }
}

/// A served connection: plain TCP or TLS.
pub trait Conn: Read + Write {
    fn peer(&self) -> Option<std::net::SocketAddr>;
    fn is_tls(&self) -> bool {
        false
    }
}

impl Conn for TcpStream {
    fn peer(&self) -> Option<std::net::SocketAddr> {
        self.peer_addr().ok()
    }
}

impl Conn for rustls::StreamOwned<rustls::ServerConnection, TcpStream> {
    fn peer(&self) -> Option<std::net::SocketAddr> {
        self.sock.peer_addr().ok()
    }
    fn is_tls(&self) -> bool {
        true
    }
}

/// Whether to also serve port 80 / HTTPS: decided when the web server starts,
/// off when another program (e.g. Laragon's Apache) already answers there.
pub static SERVE_80: AtomicBool = AtomicBool::new(false);
pub static SERVE_HTTPS: AtomicBool = AtomicBool::new(false);

/// Web server: HTTP on `web_port` (and 80 when that is free and differs),
/// HTTPS on `https_port` with the local CA's certificate.
pub fn start_http_server(running_flag: Arc<AtomicBool>, www_root: String, tunnel_manager: crate::tunnel::TunnelManager) {
    let http = |port_of: fn() -> Option<u16>| {
        let (flag, root, tm) = (running_flag.clone(), www_root.clone(), tunnel_manager.clone());
        thread::spawn(move || {
            listen(flag, port_of, |mut s| {
                let (root, tm) = (root.clone(), tm.clone());
                thread::spawn(move || {
                    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
                    let _ = s.set_write_timeout(Some(Duration::from_secs(30)));
                    handle_client(&mut s, &root, &tm);
                });
            })
        });
    };
    http(|| Some(crate::settings::current().web_port));
    http(|| Some(80).filter(|_| SERVE_80.load(Ordering::SeqCst) && crate::settings::current().web_port != 80));

    thread::spawn(move || {
        listen(running_flag, || Some(crate::settings::current().https_port).filter(|_| SERVE_HTTPS.load(Ordering::SeqCst)), |s| {
            let (root, tm) = (www_root.clone(), tunnel_manager.clone());
            thread::spawn(move || {
                let Ok(cfg) = crate::ssl::server_config(Path::new(&root)) else { return };
                let Ok(conn) = rustls::ServerConnection::new(cfg) else { return };
                let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
                let _ = s.set_write_timeout(Some(Duration::from_secs(30)));
                let mut tls = rustls::StreamOwned::new(conn, s);
                handle_client(&mut tls, &root, &tm);
                tls.conn.send_close_notify();
                let _ = tls.flush();
            });
        })
    });
}

fn handle_client(stream: &mut dyn Conn, www_root: &str, tunnel_manager: &crate::tunnel::TunnelManager) {
    let mut buffer = [0u8; 8192];
    let bytes_read = match stream.read(&mut buffer) {
        Ok(n) if n > 0 => n,
        _ => return,
    };

    let request_str = String::from_utf8_lossy(&buffer[..bytes_read]);
    let first_line = request_str.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();

    if parts.len() < 2 {
        let response = "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n";
        let _ = stream.write_all(response.as_bytes());
        return;
    }

    let method = parts[0];
    // PUT/PATCH/DELETE back REST APIs; OPTIONS is the browser's CORS preflight.
    if !matches!(method, "GET" | "POST" | "HEAD" | "PUT" | "PATCH" | "DELETE" | "OPTIONS") {
        let response = "HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n";
        let _ = stream.write_all(response.as_bytes());
        return;
    }

    let raw_url = parts[1];
    let host = request_str
        .lines()
        .find_map(|l| l.split_once(':').filter(|(k, _)| k.trim().eq_ignore_ascii_case("host")).map(|(_, v)| v.trim()))
        .unwrap_or("-");
    append_log(
        "access.log",
        &format!(
            "{} {} {} {}{}",
            crate::state::MyCraftState::current_time_str(),
            stream.peer().map(|p| p.ip().to_string()).unwrap_or_else(|| "-".into()),
            method,
            if stream.is_tls() { "https://" } else { "http://" },
            format!("{host}{raw_url}")
        ),
    );
    let (path_str, query_str) = match raw_url.find('?') {
        Some(idx) => (&raw_url[..idx], &raw_url[idx + 1..]),
        None => (raw_url, ""),
    };

    if path_str == "/logo.png" {
        let logo_bytes = include_bytes!("../assets/logo.png");
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            logo_bytes.len()
        );
        let _ = stream.write_all(response.as_bytes());
        let _ = stream.write_all(logo_bytes);
        return;
    }

    // Pinggy Tunnel API routing
    if path_str == "/api/tunnel" || path_str == "/api/tunnel/" || path_str.starts_with("/api/tunnel") {
        handle_tunnel_api(stream, query_str, tunnel_manager);
        return;
    }

    // Adminer Database Manager routing
    if path_str == "/adminer" || path_str == "/adminer/" || path_str.starts_with("/adminer/") {
        serve_adminer(stream, &request_str, &buffer[..bytes_read], path_str, query_str);
        return;
    }

    let decoded_path = percent_decode(path_str);
    let root = Path::new(www_root);
    let clean_subpath = decoded_path.trim_start_matches('/');

    if clean_subpath == "adminer" || clean_subpath.starts_with("adminer/") {
        serve_adminer(stream, &request_str, &buffer[..bytes_read], path_str, query_str);
        return;
    }

    // Check for Host header virtual host routing (*.go or custom domain)
    let host_line = request_str.lines().find(|l| {
        let trimmed = l.trim_start();
        trimmed.len() >= 5 && trimmed[..5].eq_ignore_ascii_case("host:")
    });
    let host_domain = host_line
        .map(|l| {
            let val = l.split_once(':').map(|x| x.1).unwrap_or("").trim();
            val.split(':').next().unwrap_or("").trim().to_lowercase()
        })
        .unwrap_or_default();

    // A matched project owns the whole request (PHP included); never fall
    // through to the www-root hub, or every project opens the dashboard.
    let cfg = crate::settings::current();
    let matched_vhost = cfg.extract_project_name(&host_domain)
        .or_else(|| host_domain.strip_suffix(".go"));
    if let Some(vhost_name) = matched_vhost {
        let vhost_name = vhost_name.trim();
        if !vhost_name.is_empty() && !vhost_name.starts_with('.') && !vhost_name.contains(['/', '\\']) {
            let vhost_dir = root.join(vhost_name);
            if vhost_dir.is_dir() {
                crate::tunnel::serve_project(stream, &vhost_dir, vhost_name, &request_str, &buffer[..bytes_read]);
                return;
            }
            // Fallback for wildcard subdomains (e.g. sub.project -> project)
            if let Some((_, base_project)) = vhost_name.split_once('.') {
                let base_dir = root.join(base_project);
                if base_dir.is_dir() {
                    crate::tunnel::serve_project(stream, &base_dir, base_project, &request_str, &buffer[..bytes_read]);
                    return;
                }
            }
        }
    }

    let target_path = root.join(clean_subpath);

    if clean_subpath.is_empty() || clean_subpath == "index.html" {
        let index_file = root.join("index.html");
        if index_file.is_file() {
            serve_file(stream, &index_file);
        } else {
            serve_hub_dashboard(stream, www_root, tunnel_manager);
        }
        return;
    }

    if target_path.is_dir() {
        let index_file = target_path.join("index.html");
        let index_htm = target_path.join("index.htm");
        if index_file.is_file() {
            serve_file(stream, &index_file);
            return;
        } else if index_htm.is_file() {
            serve_file(stream, &index_htm);
            return;
        } else {
            let redirect_url = format!("http://{}", cfg.project_hostname(clean_subpath));
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                redirect_url
            );
            let _ = stream.write_all(response.as_bytes());
            return;
        }
    }

    if target_path.is_file() {
        serve_file(stream, &target_path);
    } else {
        serve_not_found(stream, path_str);
    }
}

static LOG_LOCK: Mutex<()> = Mutex::new(());

/// Append one line to `<root>\logs\<file>` (access.log, php_error.log).
pub fn append_log(file: &str, line: &str) {
    let _guard = LOG_LOCK.lock();
    let dir = crate::mycraft_root().join("logs");
    let _ = fs::create_dir_all(&dir);
    if let Ok(mut f) = fs::OpenOptions::new().create(true).append(true).open(dir.join(file)) {
        let _ = writeln!(f, "{line}");
    }
}

pub(crate) fn percent_decode(input: &str) -> String {
    let mut result = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(val) = u8::from_str_radix(&input[i + 1..i + 3], 16) {
                result.push(val);
                i += 3;
                continue;
            }
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&result).to_string()
}

fn serve_file(stream: &mut dyn Conn, file_path: &Path) {
    if let Ok(contents) = fs::read(file_path) {
        let mime = get_mime_type(file_path);
        let headers = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            mime,
            contents.len()
        );
        let _ = stream.write_all(headers.as_bytes());
        let _ = stream.write_all(&contents);
    } else {
        serve_not_found(stream, &file_path.to_string_lossy());
    }
}

fn get_mime_type(path: &Path) -> &'static str {
    match path.extension().and_then(|s| s.to_str()).unwrap_or("") {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "application/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

fn handle_tunnel_api(
    stream: &mut dyn Conn,
    query_str: &str,
    tunnel_manager: &crate::tunnel::TunnelManager,
) {
    let mut action = "status";
    let mut project = "";

    for pair in query_str.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == "action" {
                action = v;
            } else if k == "project" {
                project = v;
            }
        }
    }

    match action {
        "start" => {
            if !project.is_empty() {
                let _ = tunnel_manager.start_tunnel(project);
            }
        }
        "stop" => {
            if !project.is_empty() {
                tunnel_manager.stop_tunnel(project);
            }
        }
        _ => {}
    }

    let tunnels = tunnel_manager.list_tunnels();
    let mut items = Vec::new();
    for (name, status) in tunnels {
        let (st_str, url_str, port_val) = match status {
            crate::tunnel::TunnelStatus::Starting => ("starting", String::new(), 0),
            crate::tunnel::TunnelStatus::Online { url, local_port } => ("online", url, local_port),
            crate::tunnel::TunnelStatus::Error(e) => ("error", e, 0),
        };
        items.push(format!(
            r#"{{"project":"{}","status":"{}","url":"{}","port":{}}}"#,
            escape_json(&name),
            st_str,
            escape_json(&url_str),
            port_val
        ));
    }

    let json = format!(r#"{{"tunnels":[{}]}}"#, items.join(","));
    let resp = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nAccess-Control-Allow-Origin: *\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        json.len(),
        json
    );
    let _ = stream.write_all(resp.as_bytes());
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\r', "\\r")
}

/// One badge per service, with its real install and whether its port answers.
fn stack_items() -> String {
    let cfg = crate::settings::current();
    let newest = |section: &str| crate::packages::installed(section).into_iter().next();
    let rows: [(&str, Option<String>, Option<u16>, String); 6] = [
        ("Web server", Some("built-in".into()), Some(cfg.web_port), format!("HTTP {} / HTTPS {}", cfg.web_port, cfg.https_port)),
        ("MySQL / MariaDB", newest("mysql"), Some(cfg.mysql_port), format!("Port {}", cfg.mysql_port)),
        ("PHP", newest("php"), None, "per request (php-cgi)".into()),
        ("Node.js", newest("nodejs"), None, "per project".into()),
        ("Redis", newest("redis"), Some(cfg.redis_port), format!("Port {}", cfg.redis_port)),
        ("Mailpit", Some("built-in".into()), Some(cfg.mailpit_port), format!("Web {} / SMTP {}", cfg.mailpit_port, cfg.smtp_port)),
    ];
    rows.iter()
        .map(|(name, install, port, detail)| {
            let status = match (install, port) {
                (None, _) => "NOT INSTALLED",
                (Some(_), None) => "ON DEMAND",
                (Some(_), Some(p)) if crate::mysql::port_open(*p) => "RUNNING",
                _ => "STOPPED",
            };
            format!(
                r#"<div class="stack-badge"><div class="stack-status">{status}</div><div class="stack-name">{name}</div><div class="stack-port">{} • {detail}</div></div>"#,
                install.as_deref().unwrap_or("—")
            )
        })
        .collect()
}

fn serve_hub_dashboard(stream: &mut dyn Conn, www_root: &str, _tunnel_manager: &crate::tunnel::TunnelManager) {
    let cfg = crate::settings::current();
    let stack_items = stack_items();
    let version = env!("CARGO_PKG_VERSION");
    let mut project_items = String::new();
    let root = Path::new(www_root);

    if let Ok(entries) = fs::read_dir(root) {
        let mut dir_names: Vec<String> = Vec::new();
        for entry in entries.flatten() {
            if let Ok(ft) = entry.file_type() {
                if ft.is_dir() {
                    let folder_name = entry.file_name().to_string_lossy().to_string();
                    // Exclude any folders that start with dot (e.g. .claude, .pnpm-store) or underscore
                    if folder_name.starts_with('.') || folder_name.starts_with('_') {
                        continue;
                    }
                    dir_names.push(folder_name);
                }
            }
        }
        dir_names.sort();

        for folder_name in dir_names {
            let stack_hint = if folder_name == "mycraft" {
                "Rust / Axum"
            } else {
                "PHP 8.3"
            };
            let host_url = format!("http://{}", cfg.project_hostname(&folder_name));
            let item_html = format!(
                r#"<div class="project-card" data-name="{name}">
                    <div class="project-info">
                        <span class="project-icon">📁</span>
                        <div>
                            <h3 class="project-title"><a href="{host_url}">{name}</a></h3>
                            <div style="display:flex; align-items:center; gap:0.5rem; margin-top:0.25rem;">
                                <span class="project-domain">{host_url} &bull; Local DNS: 127.0.0.1</span>
                                <span style="background: rgba(56, 189, 248, 0.15); color: #38bdf8; border: 1px solid rgba(56, 189, 248, 0.3); padding: 0.1rem 0.45rem; border-radius: 4px; font-size: 0.75rem; font-weight: 600;">⚡ {stack}</span>
                            </div>
                            <div id="tunnel-badge-{name}" class="tunnel-badge" style="display:none; margin-top:0.35rem; font-size:0.8rem;"></div>
                        </div>
                    </div>
                    <div style="display:flex; align-items:center; gap:0.5rem;">
                        <button class="open-btn tunnel-btn" id="tunnel-btn-{name}" onclick="toggleTunnel('{name}')" style="background: rgba(56, 189, 248, 0.12); color: #38bdf8; border: 1px solid rgba(56, 189, 248, 0.35); cursor:pointer;">🌐 Make Online</button>
                        <a class="open-btn" href="{host_url}">Open Project →</a>
                    </div>
                </div>"#,
                name = folder_name,
                host_url = host_url,
                stack = stack_hint,
            );
            project_items.push_str(&item_html);
        }
    }

    if project_items.is_empty() {
        project_items = r#"<div class="empty-state">
            <p>No project folders found in www root yet.</p>
            <p>Use <strong>Quick App</strong> in the Mycraft window to scaffold your first project!</p>
        </div>"#.to_string();
    }

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Mycraft - Full Stack Dev Box</title>
    <style>
        :root {{
            --bg-dark: #0f172a;
            --surface: #1e293b;
            --surface-hover: #334155;
            --border: #334155;
            --primary: #38bdf8;
            --success: #10b981;
            --text-main: #f8fafc;
            --text-muted: #94a3b8;
        }}
        * {{ box-sizing: border-box; }}
        body {{
            font-family: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
            background-color: var(--bg-dark);
            color: var(--text-main);
            margin: 0;
            padding: 0;
            min-height: 100vh;
        }}
        .header {{
            background: linear-gradient(135deg, #1e293b 0%, #0f172a 100%);
            border-bottom: 1px solid var(--border);
            padding: 2rem;
            text-align: center;
        }}
        .brand-badge {{
            display: inline-block;
            background: rgba(56, 189, 248, 0.15);
            color: var(--primary);
            border: 1px solid rgba(56, 189, 248, 0.3);
            padding: 0.35rem 0.85rem;
            border-radius: 9999px;
            font-size: 0.85rem;
            font-weight: 600;
            margin-bottom: 0.75rem;
        }}
        h1 {{ margin: 0 0 0.5rem; font-size: 2.25rem; font-weight: 800; }}
        p.subtitle {{ color: var(--text-muted); margin: 0 auto; max-width: 600px; font-size: 1rem; }}
        .container {{
            max-width: 900px;
            margin: 2rem auto;
            padding: 0 1.5rem;
        }}
        .stack-grid {{
            display: grid;
            grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
            gap: 1rem;
            margin-bottom: 2.5rem;
        }}
        .stack-badge {{
            background: var(--surface);
            border: 1px solid var(--border);
            border-radius: 0.75rem;
            padding: 1rem;
            text-align: center;
        }}
        .stack-status {{
            color: var(--success);
            font-size: 0.8rem;
            font-weight: bold;
            display: flex;
            align-items: center;
            justify-content: center;
            gap: 0.35rem;
            margin-bottom: 0.35rem;
        }}
        .stack-status::before {{
            content: "●";
            font-size: 0.9rem;
        }}
        .stack-name {{ font-weight: 700; font-size: 1rem; margin-bottom: 0.2rem; }}
        .stack-port {{ color: var(--text-muted); font-size: 0.8rem; font-family: monospace; }}
        .section-header {{
            display: flex;
            justify-content: space-between;
            align-items: center;
            margin-bottom: 1rem;
            border-bottom: 1px solid var(--border);
            padding-bottom: 0.5rem;
        }}
        .section-title {{ font-size: 1.25rem; font-weight: 700; margin: 0; }}
        .project-list {{
            display: flex;
            flex-direction: column;
            gap: 0.75rem;
        }}
        .project-card {{
            background: var(--surface);
            border: 1px solid var(--border);
            border-radius: 0.75rem;
            padding: 1rem 1.25rem;
            display: flex;
            justify-content: space-between;
            align-items: center;
            transition: all 0.2s ease;
        }}
        .project-card:hover {{
            background: var(--surface-hover);
            border-color: var(--primary);
        }}
        .project-info {{
            display: flex;
            align-items: center;
            gap: 1rem;
        }}
        .project-icon {{ font-size: 1.5rem; }}
        .project-title {{ margin: 0 0 0.15rem; font-size: 1.05rem; }}
        .project-title a {{ color: var(--text-main); text-decoration: none; }}
        .project-domain {{ color: var(--text-muted); font-size: 0.85rem; font-family: monospace; }}
        .open-btn {{
            background: rgba(56, 189, 248, 0.15);
            color: var(--primary);
            text-decoration: none;
            padding: 0.5rem 1rem;
            border-radius: 0.5rem;
            font-size: 0.9rem;
            font-weight: 600;
        }}
        .open-btn:hover {{
            background: var(--primary);
            color: var(--bg-dark);
        }}
        .empty-state {{
            background: var(--surface);
            border: 1px dashed var(--border);
            padding: 2.5rem;
            border-radius: 0.75rem;
            text-align: center;
            color: var(--text-muted);
        }}
    </style>
</head>
<body>
    <div class="header">
        <img src="/logo.png" alt="Mycraft Logo" style="width: 54px; height: 50px; margin-bottom: 0.6rem; image-rendering: pixelated;">
        <br>
        <span class="brand-badge">MYCRAFT {version}</span>
        <h1>Welcome to Mycraft</h1>
        <p class="subtitle">Services below are checked live on each page load.</p>
    </div>

    <div class="container">
        <div class="section-header">
            <h2 class="section-title">Services</h2>
        </div>
        <div class="stack-grid">
            {stack_items}
        </div>

        <div class="section-header" style="flex-wrap: wrap; gap: 0.75rem;">
            <div>
                <h2 class="section-title">Local Projects (www root)</h2>
                <span style="color: var(--text-muted); font-size: 0.85rem; font-family: monospace;">{www_root}</span>
            </div>
            <div style="flex: 1; max-width: 320px; min-width: 220px;">
                <input type="text" id="projectSearch" placeholder="🔍 Search projects by name..." oninput="filterProjects()" style="width: 100%; padding: 0.55rem 0.9rem; border-radius: 0.5rem; background: var(--surface); border: 1px solid var(--border); color: var(--text-main); font-size: 0.9rem; outline: none; box-sizing: border-box; transition: border-color 0.2s;" onfocus="this.style.borderColor='var(--primary)'" onblur="this.style.borderColor='var(--border)'">
            </div>
        </div>
        <div class="project-list" id="projectList">
            {project_items}
        </div>
        <div id="noMatchNotice" style="display:none; background: var(--surface); border: 1px dashed var(--border); padding: 2rem; border-radius: 0.75rem; text-align: center; color: var(--text-muted); margin-top: 0.5rem;">
            <p style="margin: 0;">No projects match your search filter.</p>
        </div>
    </div>
    <script>
        function filterProjects() {{
            const query = (document.getElementById('projectSearch').value || '').toLowerCase().trim();
            const cards = document.querySelectorAll('.project-card');
            let visible = 0;
            cards.forEach(card => {{
                const name = (card.getAttribute('data-name') || '').toLowerCase();
                if (!query || name.includes(query)) {{
                    card.style.display = 'flex';
                    visible++;
                }} else {{
                    card.style.display = 'none';
                }}
            }});
            const notice = document.getElementById('noMatchNotice');
            if (notice) {{
                notice.style.display = (visible === 0 && cards.length > 0) ? 'block' : 'none';
            }}
        }}

        function toggleTunnel(project) {{
            const btn = document.getElementById('tunnel-btn-' + project);
            if (btn && btn.dataset.running === 'true') {{
                fetch('/api/tunnel?action=stop&project=' + encodeURIComponent(project))
                    .then(() => pollTunnels());
            }} else {{
                if (btn) {{
                    btn.innerText = '⏳ Connecting...';
                    btn.disabled = true;
                }}
                fetch('/api/tunnel?action=start&project=' + encodeURIComponent(project))
                    .then(() => {{
                        setTimeout(pollTunnels, 600);
                    }});
            }}
        }}

        function pollTunnels() {{
            fetch('/api/tunnel?action=status')
                .then(r => r.json())
                .then(data => {{
                    const onlineMap = {{}};
                    (data.tunnels || []).forEach(t => {{
                        onlineMap[t.project] = t;
                    }});

                    document.querySelectorAll('.project-card').forEach(card => {{
                        const name = card.getAttribute('data-name');
                        const btn = document.getElementById('tunnel-btn-' + name);
                        const badge = document.getElementById('tunnel-badge-' + name);
                        if (!btn || !badge) return;

                        const t = onlineMap[name];
                        if (t && t.status === 'online') {{
                            btn.dataset.running = 'true';
                            btn.innerText = '⏹ Stop Online';
                            btn.disabled = false;
                            btn.style.background = 'rgba(239, 68, 68, 0.15)';
                            btn.style.color = '#ef4444';
                            btn.style.borderColor = 'rgba(239, 68, 68, 0.4)';

                            badge.style.display = 'block';
                            badge.innerHTML = '<span style="color:#10b981; font-weight:600;">🟢 Online:</span> <a href="' + t.url + '" target="_blank" style="color:#38bdf8; text-decoration:underline; font-weight:500;">' + t.url + '</a>';
                        }} else if (t && t.status === 'starting') {{
                            btn.dataset.running = 'false';
                            btn.innerText = '⏳ Connecting...';
                            btn.disabled = true;
                            badge.style.display = 'block';
                            badge.innerHTML = '<span style="color:#f59e0b;">⏳ Launching Pinggy SSH tunnel...</span>';
                        }} else if (t && t.status === 'error') {{
                            btn.dataset.running = 'false';
                            btn.innerText = '🔄 Retry Online';
                            btn.disabled = false;
                            badge.style.display = 'block';
                            badge.innerHTML = '<span style="color:#ef4444;">⚠ ' + (t.url || 'Tunnel Error') + '</span>';
                        }} else {{
                            btn.dataset.running = 'false';
                            btn.innerText = '🌐 Make Online';
                            btn.disabled = false;
                            btn.style.background = 'rgba(56, 189, 248, 0.12)';
                            btn.style.color = '#38bdf8';
                            btn.style.borderColor = 'rgba(56, 189, 248, 0.35)';
                            badge.style.display = 'none';
                            badge.innerHTML = '';
                        }}
                    }});
                }}).catch(() => {{}});
        }}

        setInterval(pollTunnels, 2500);
        pollTunnels();
    </script>
</body>
</html>"#,
        www_root = www_root,
        project_items = project_items
    );

    let headers = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        html.len()
    );
    let _ = stream.write_all(headers.as_bytes());
    let _ = stream.write_all(html.as_bytes());
}

fn serve_not_found(stream: &mut dyn Conn, path: &str) {
    let body = format!(
        "<!DOCTYPE html><html><body style='background:#0f172a;color:#f8fafc;font-family:sans-serif;text-align:center;padding:3rem;'><h1>404 Not Found</h1><p>Resource <code>{}</code> does not exist.</p><a style='color:#38bdf8;' href='/'>Back to Mycraft Hub</a></body></html>",
        path
    );
    let headers = format!(
        "HTTP/1.1 404 Not Found\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(headers.as_bytes());
    let _ = stream.write_all(body.as_bytes());
}

fn serve_adminer(
    stream: &mut dyn Conn,
    request_str: &str,
    raw_buffer: &[u8],
    path_str: &str,
    query_str: &str,
) {
    if path_str == "/adminer" {
        let redirect = "HTTP/1.1 301 Moved Permanently\r\nLocation: /adminer/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let _ = stream.write_all(redirect.as_bytes());
        return;
    }

    let laragon_root = crate::mycraft_root();
    let adminer_php = laragon_root
        .join("etc")
        .join("apps")
        .join("adminer")
        .join("index.php");

    // Look for php-cgi.exe (prefer PHP 7.1.33 compatible with Adminer 4.6.3, or any php-cgi)
    let php_bin_dir = laragon_root.join("bin").join("php");
    let mut php_cgi_path = None;
    if let Ok(entries) = fs::read_dir(&php_bin_dir) {
        let mut candidates: Vec<std::path::PathBuf> = Vec::new();
        for entry in entries.flatten() {
            let p = entry.path().join("php-cgi.exe");
            if p.is_file() {
                candidates.push(p);
            }
        }
        if let Some(p7) = candidates
            .iter()
            .find(|p| p.to_string_lossy().contains("7.1") || p.to_string_lossy().contains("7."))
        {
            php_cgi_path = Some(p7.clone());
        } else if let Some(any_php) = candidates.first() {
            php_cgi_path = Some(any_php.clone());
        }
    }

    if let (Some(cgi), true) = (php_cgi_path, adminer_php.is_file()) {
        let method = request_str
            .lines()
            .next()
            .unwrap_or("")
            .split_whitespace()
            .next()
            .unwrap_or("GET");
        let cookie = request_str
            .lines()
            .find(|l| l.to_lowercase().starts_with("cookie:"))
            .map(|l| l.split_once(':').map(|x| x.1.trim()).unwrap_or(""))
            .unwrap_or("");
        let content_type = request_str
            .lines()
            .find(|l| l.to_lowercase().starts_with("content-type:"))
            .map(|l| l.split_once(':').map(|x| x.1.trim()).unwrap_or(""))
            .unwrap_or("");
        let content_length = request_str
            .lines()
            .find(|l| l.to_lowercase().starts_with("content-length:"))
            .map(|l| l.split_once(':').map(|x| x.1.trim()).unwrap_or(""))
            .unwrap_or("0");

        let header_end = raw_buffer
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|p| p + 4);
        let body = header_end.map(|idx| &raw_buffer[idx..]).unwrap_or(&[]);

        let req_uri = if query_str.is_empty() {
            path_str.to_string()
        } else {
            format!("{}?{}", path_str, query_str)
        };

        let mut cmd = Command::new(&cgi);
        crate::hide_console(&mut cmd);
        cmd.env("REDIRECT_STATUS", "200")
            .env("GATEWAY_INTERFACE", "CGI/1.1")
            .env("SERVER_PROTOCOL", "HTTP/1.1")
            .env("REQUEST_METHOD", method)
            .env("SCRIPT_FILENAME", &adminer_php)
            .env("SCRIPT_NAME", "/adminer/index.php")
            .env("REQUEST_URI", req_uri)
            .env("QUERY_STRING", query_str)
            .env("HTTP_COOKIE", cookie)
            .env("CONTENT_TYPE", content_type)
            .env("CONTENT_LENGTH", content_length);

        if method == "POST" && !body.is_empty() {
            use std::process::Stdio;
            cmd.stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if let Ok(mut child) = cmd.spawn() {
                if let Some(mut stdin) = child.stdin.take() {
                    let _ = stdin.write_all(body);
                }
                if let Ok(output) = child.wait_with_output() {
                    if output.status.success() && !output.stdout.is_empty() {
                        let stdout_str = String::from_utf8_lossy(&output.stdout);
                        let mut resp = if !stdout_str
                            .lines()
                            .any(|l| l.to_lowercase().starts_with("status:"))
                        {
                            b"HTTP/1.1 200 OK\r\n".to_vec()
                        } else {
                            let status_line = stdout_str
                                .lines()
                                .find(|l| l.to_lowercase().starts_with("status:"))
                                .unwrap_or("Status: 200 OK");
                            let status_val = status_line
                                .split_once(':')
                                .map(|x| x.1.trim())
                                .unwrap_or("200 OK");
                            format!("HTTP/1.1 {}\r\n", status_val).into_bytes()
                        };
                        resp.extend_from_slice(&output.stdout);
                        let _ = stream.write_all(&resp);
                        return;
                    }
                }
            }
        } else if let Ok(output) = cmd.output() {
            if output.status.success() && !output.stdout.is_empty() {
                let stdout_str = String::from_utf8_lossy(&output.stdout);
                let mut resp = if !stdout_str
                    .lines()
                    .any(|l| l.to_lowercase().starts_with("status:"))
                {
                    b"HTTP/1.1 200 OK\r\n".to_vec()
                } else {
                    let status_line = stdout_str
                        .lines()
                        .find(|l| l.to_lowercase().starts_with("status:"))
                        .unwrap_or("Status: 200 OK");
                    let status_val = status_line
                        .split_once(':')
                        .map(|x| x.1.trim())
                        .unwrap_or("200 OK");
                    format!("HTTP/1.1 {}\r\n", status_val).into_bytes()
                };
                resp.extend_from_slice(&output.stdout);
                let _ = stream.write_all(&resp);
                return;
            }
        }
    }

    serve_adminer_fallback(stream);
}

/// Adminer isn't installed: say so, with the live database status.
fn serve_adminer_fallback(stream: &mut dyn Conn) {
    let port = crate::settings::current().mysql_port;
    let (status, color) = if crate::mysql::port_open(port) {
        (format!("Something is listening on 127.0.0.1:{port}"), "#10b981")
    } else {
        (format!("Nothing is listening on 127.0.0.1:{port}: start MySQL / MariaDB in myCraft"), "#f59e0b")
    };
    let path = crate::mycraft_root().join("etc").join("apps").join("adminer").join("index.php");
    let html = format!(
        r#"<!DOCTYPE html><html lang="en"><head><meta charset="utf-8"><title>Adminer not installed</title></head>
<body style="background:#0f172a;color:#e2e8f0;font-family:system-ui,sans-serif;display:grid;place-items:center;min-height:100vh;margin:0">
<div style="max-width:560px;padding:2rem;background:#1e293b;border:1px solid #334155;border-radius:12px">
<h1 style="margin-top:0">Adminer is not installed</h1>
<p>myCraft serves Adminer from <code>{}</code>, and that file doesn't exist. Put Adminer's <code>adminer-*.php</code> there as <code>index.php</code>, or use HeidiSQL (myCraft's ⚙ MySQL → Open HeidiSQL).</p>
<p style="color:{color}">{status}.</p>
</div></body></html>"#,
        path.display()
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        html.len(),
        html
    );
    let _ = stream.write_all(response.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Real HTTP + HTTPS round trip on ports 18080 / 18443 against a scratch www.
    /// Creates the CA in <root>\etc\ssl. `cargo test -- --ignored live_web --nocapture`
    #[test]
    #[ignore]
    fn live_web() {
        use rustls::pki_types::{pem::PemObject, CertificateDer};
        let www = std::env::temp_dir().join("mycraft-live-web");
        fs::create_dir_all(www.join("demo")).unwrap();
        fs::write(www.join("demo").join("index.html"), "hello demo").unwrap();
        crate::settings::set_for_test(crate::settings::Settings { web_port: 18080, https_port: 18443, ..Default::default() });
        SERVE_HTTPS.store(true, Ordering::SeqCst);
        let root = www.to_string_lossy().to_string();
        start_http_server(Arc::new(AtomicBool::new(true)), root.clone(), crate::tunnel::TunnelManager::new(root));
        thread::sleep(Duration::from_millis(500));

        // Trust only our CA, and resolve every name to 127.0.0.1.
        let ca = CertificateDer::from_pem_file(crate::ssl::ensure_ca().unwrap()).unwrap();
        let mut roots = rustls::RootCertStore::empty();
        roots.add(ca).unwrap();
        let tls = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let agent = ureq::AgentBuilder::new()
            .tls_config(Arc::new(tls))
            .resolver(|addr: &str| {
                let port: u16 = addr.rsplit(':').next().unwrap_or("80").parse().unwrap_or(80);
                Ok(vec![std::net::SocketAddr::from(([127, 0, 0, 1], port))])
            })
            .build();
        for url in ["http://demo.go:18080/", "https://demo.go:18443/"] {
            let body = agent.get(url).call().unwrap_or_else(|e| panic!("{url}: {e}")).into_string().unwrap();
            println!("{url} -> {body}");
            assert_eq!(body, "hello demo");
        }
        let log = fs::read_to_string(crate::mycraft_root().join("logs").join("access.log")).unwrap();
        assert!(log.contains("https://demo.go:18443/"));
    }
}
