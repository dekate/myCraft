use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use crate::tunnel::{TunnelManager, TunnelStatus};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ActiveTab {
    Dashboard,
    QuickApp,
    VirtualHosts,
    Ai,
    Logs,
    Settings,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LogLevel {
    Info,
    Success,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub struct LogEntry {
    pub timestamp: String,
    pub level: LogLevel,
    pub message: String,
}

#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ToastKind {
    Success,
    Info,
    Warning,
    Error,
}

#[derive(Clone, Debug)]
pub struct Toast {
    pub id: u64,
    pub message: String,
    pub kind: ToastKind,
    pub created_at: std::time::Instant,
    pub duration: std::time::Duration,
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct Service {
    pub name: String,
    pub service_type: String,
    /// `bin\<section>` holding this service's installs; `None` = built into myCraft.
    pub bin_section: Option<&'static str>,
    pub version: String,
    pub available_versions: Vec<String>,
    pub port: u16,
    pub aux_port: Option<u16>,
    pub running: bool,
    pub config_file: String,
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct QuickTemplate {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub category: &'static str,
    pub badge: &'static str,
}

#[allow(dead_code)]
#[derive(Clone, Debug)]
pub struct VirtualHost {
    pub name: String,
    pub domain: String,
    pub root_folder: String,
    pub port: u16,
    pub ssl_enabled: bool,
}

#[derive(Clone, Debug)]
pub struct ConfigModal {
    pub is_open: bool,
    pub title: String,
    pub file_path: String,
    pub content: String,
}

pub struct MyCraftState {
    pub services: Vec<Service>,
    pub logs: Vec<LogEntry>,
    pub active_tab: ActiveTab,
    pub menu_open: bool,
    pub all_running: bool,
    pub www_root: String,
    pub web_port: u16,
    pub mysql_port: u16,
    pub redis_port: u16,
    pub mailpit_port: u16,
    pub https_port: u16,
    pub smtp_port: u16,
    pub auto_vhosts: bool,
    pub domain: String,
    /// Mirrors the Windows Run key (read at startup, written on Save).
    pub auto_start: bool,
    /// Closing the window hides it to the system tray; tray or Menu → Exit really quits.
    pub close_to_tray: bool,
    /// Send OS notifications for Quick App, AI completion, service events.
    pub notifications: bool,
    pub new_app_name: String,
    pub selected_template: usize,
    pub config_modal: ConfigModal,
    pub service_options_idx: Option<usize>,
    pub server_running_flag: Arc<AtomicBool>,
    pub mailpit_running_flag: Arc<AtomicBool>,
    pub is_admin: bool,
    pub dns_wired: bool,
    pub dns_check_message: String,
    pub vhost_search: String,
    pub db_tool_preference: String,
    pub terminal_preference: String,
    pub quick_create_status: Option<String>,
    pub features_modal_open: bool,
    pub tunnel_manager: TunnelManager,
    pub ai: crate::ai::AiChat,
    pub php_updater: crate::php_update::PhpUpdater,
    pub installer: crate::packages::Installer,
    pub mysql: crate::mysql::MySql,
    pub mysql_status: crate::mysql::DbStatus,
    pub redis: crate::redis::Redis,
    pub redis_status: crate::mysql::DbStatus,
    /// Results from worker threads (Quick App, hosts, trusting the CA), drained each frame.
    pub bg_tx: std::sync::mpsc::Sender<crate::quickapp::Note>,
    bg_rx: std::sync::mpsc::Receiver<crate::quickapp::Note>,
    pub quick_busy: bool,
    pub ssl_trusted: bool,
    pub toasts: Vec<Toast>,
    pub next_toast_id: u64,
    pub button_loading: std::collections::HashMap<String, std::time::Instant>,
}

impl Default for MyCraftState {
    fn default() -> Self {
        let www_path = crate::mycraft_root().join("www").to_string_lossy().to_string();

        let cfg = crate::settings::current();
        let is_admin = Self::check_is_admin();
        let (dns_wired, dns_msg) = Self::check_dns_wired(&www_path, &cfg.domain);
        let mailpit_running_flag = Arc::new(AtomicBool::new(false));

        // Versions are whatever is really in bin\<section>; the web server and
        // mail catcher are built into myCraft.
        let svc = |name: &str, kind: &str, section: Option<&'static str>, port: u16, aux_port: Option<u16>, config: &str| Service {
            name: name.into(),
            service_type: kind.into(),
            bin_section: section,
            version: String::new(),
            available_versions: Vec::new(),
            port,
            aux_port,
            running: false,
            config_file: config.into(),
        };
        let mut services = vec![
            svc("Web Server", "HTTP / HTTPS", None, cfg.web_port, Some(cfg.https_port), ""),
            svc("MySQL / MariaDB", "SQL Database", Some("mysql"), cfg.mysql_port, None, "my.ini"),
            svc("PHP", "Runs per request (php-cgi)", Some("php"), 0, None, ""),
            svc("Node.js", "Runs per project", Some("nodejs"), 0, None, ""),
            svc("Redis", "In-Memory Store", Some("redis"), cfg.redis_port, None, "redis.conf"),
            svc("Mailpit", "Email Testing", None, cfg.mailpit_port, Some(cfg.smtp_port), ""),
        ];
        Self::detect_versions(&mut services);


        let (bg_tx, bg_rx) = std::sync::mpsc::channel();
        let mut s = Self {
            services,
            logs: Vec::new(),
            active_tab: ActiveTab::Dashboard,
            menu_open: false,
            all_running: false,
            www_root: www_path.clone(),
            web_port: cfg.web_port,
            https_port: cfg.https_port,
            smtp_port: cfg.smtp_port,
            mysql_port: cfg.mysql_port,
            redis_port: cfg.redis_port,
            mailpit_port: cfg.mailpit_port,
            auto_vhosts: cfg.auto_vhosts,
            domain: cfg.domain.clone(),
            auto_start: crate::settings::autostart_enabled(),
            close_to_tray: cfg.close_to_tray,
            notifications: cfg.notifications,
            new_app_name: "mycraft".into(),
            selected_template: 0,
            config_modal: ConfigModal {
                is_open: false,
                title: String::new(),
                file_path: String::new(),
                content: String::new(),
            },
            service_options_idx: None,
            server_running_flag: Arc::new(AtomicBool::new(false)),
            mailpit_running_flag,
            is_admin,
            dns_wired,
            dns_check_message: dns_msg,
            vhost_search: String::new(),
            db_tool_preference: cfg.db_tool.clone(),
            terminal_preference: cfg.terminal.clone(),
            quick_create_status: None,
            features_modal_open: false,
            tunnel_manager: TunnelManager::new(www_path.clone()),
            ai: crate::ai::AiChat::new(&www_path),
            php_updater: Default::default(),
            installer: Default::default(),
            mysql: Default::default(),
            mysql_status: crate::mysql::DbStatus::Stopped,
            redis: Default::default(),
            redis_status: crate::mysql::DbStatus::Stopped,
            bg_tx,
            bg_rx,
            quick_busy: false,
            ssl_trusted: crate::ssl::is_trusted(),
            toasts: Vec::new(),
            next_toast_id: 0,
            button_loading: std::collections::HashMap::new(),
        };

        s.log(LogLevel::Info, &format!("myCraft {} started in {}.", env!("CARGO_PKG_VERSION"), crate::mycraft_root().display()));
        if is_admin {
            s.log(LogLevel::Success, "Administrator privileges detected.");
        } else {
            let suffix = crate::settings::extract_suffix(&s.domain);
            s.log(LogLevel::Warning, &format!("Running as standard user (Not Admin). *{suffix} DNS wiring requires elevation."));
        }
        let dns_msg = s.dns_check_message.clone();
        s.log(if dns_wired { LogLevel::Success } else { LogLevel::Warning }, &dns_msg);
        // Auto virtual hosts: add missing entries now when hosts is writable.
        if !dns_wired && s.auto_vhosts && is_admin {
            s.wire_dns();
        }
        s
    }
}

#[cfg(windows)]
const HOSTS: &str = "C:\\Windows\\System32\\drivers\\etc\\hosts";
#[cfg(not(windows))]
const HOSTS: &str = "/etc/hosts";

impl MyCraftState {
    /// Fill each service's version list from what is installed in bin\.
    pub fn detect_versions(services: &mut [Service]) {
        for s in services {
            match s.bin_section {
                None => {
                    s.version = "myCraft built-in".into();
                    s.available_versions.clear();
                }
                Some(section) => {
                    s.available_versions = crate::packages::installed(section);
                    if !s.available_versions.contains(&s.version) {
                        s.version = s.available_versions.first().cloned().unwrap_or_else(|| "Not installed".into());
                    }
                }
            }
        }
    }

    /// The myCraft root (see `crate::mycraft_root`).
    pub fn root_dir(&self) -> String {
        crate::mycraft_root().to_string_lossy().to_string()
    }

    pub fn current_time_str() -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let hours = (now / 3600) % 24;
        let mins = (now / 60) % 60;
        let secs = now % 60;
        format!("{:02}:{:02}:{:02}", hours, mins, secs)
    }

    pub fn log(&mut self, level: LogLevel, message: &str) {
        self.logs.push(LogEntry {
            timestamp: Self::current_time_str(),
            level,
            message: message.to_string(),
        });
        if self.logs.len() > 250 {
            self.logs.remove(0);
        }
    }

    pub fn toast(&mut self, kind: ToastKind, message: impl Into<String>) {
        self.next_toast_id = self.next_toast_id.wrapping_add(1);
        self.toasts.push(Toast {
            id: self.next_toast_id,
            message: message.into(),
            kind,
            created_at: std::time::Instant::now(),
            duration: std::time::Duration::from_secs(3),
        });
    }

    pub fn toast_success(&mut self, message: impl Into<String>) {
        self.toast(ToastKind::Success, message);
    }

    pub fn toast_error(&mut self, message: impl Into<String>) {
        self.toast(ToastKind::Error, message);
    }

    #[allow(dead_code)]
    pub fn toast_info(&mut self, message: impl Into<String>) {
        self.toast(ToastKind::Info, message);
    }

    #[allow(dead_code)]
    pub fn toast_warning(&mut self, message: impl Into<String>) {
        self.toast(ToastKind::Warning, message);
    }

    pub fn is_loading(&self, id: &str) -> bool {
        self.button_loading.get(id).map_or(false, |until| *until > std::time::Instant::now())
    }

    pub fn set_loading(&mut self, id: &str, duration_ms: u64) {
        let until = std::time::Instant::now() + std::time::Duration::from_millis(duration_ms);
        self.button_loading.insert(id.to_string(), until);
    }

    pub fn start_tunnel(&mut self, project: &str) {
        self.log(LogLevel::Info, &format!("Launching Pinggy SSH tunnel for project '{}'...", project));
        match self.tunnel_manager.start_tunnel(project) {
            Ok(_) => {
                self.log(LogLevel::Success, &format!("Pinggy tunnel started for '{}'. Waiting for public URL...", project));
            }
            Err(e) => {
                self.log(LogLevel::Error, &format!("Failed to start Pinggy tunnel for '{}': {}", project, e));
            }
        }
    }

    pub fn stop_tunnel(&mut self, project: &str) {
        self.tunnel_manager.stop_tunnel(project);
        self.log(LogLevel::Info, &format!("Pinggy tunnel stopped for project '{}'.", project));
    }

    pub fn get_tunnel_status(&self, project: &str) -> Option<TunnelStatus> {
        self.tunnel_manager.get_status(project)
    }

    /// Services myCraft starts and stops. PHP runs per request (CGI) and
    /// Node.js per project, so neither has a daemon.
    pub fn can_run(s: &Service) -> bool {
        matches!(s.bin_section, None | Some("mysql") | Some("redis"))
    }

    pub fn start_all(&mut self) {
        for idx in 0..self.services.len() {
            if Self::can_run(&self.services[idx]) && !self.services[idx].running {
                self.set_running(idx, true);
            }
        }
        if self.all_running {
            crate::notify::send("myCraft", "All services has successfully started");
        }
    }

    pub fn stop_all(&mut self) {
        for idx in 0..self.services.len() {
            if self.services[idx].running {
                self.set_running(idx, false);
            }
        }
    }

    /// Called once when the app really quits: stops any service still online and
    /// every public tunnel, so nothing keeps running or holding ports after exit.
    pub fn shutdown(&mut self) {
        self.stop_all();
        self.tunnel_manager.stop_all();
    }

    pub fn toggle_service(&mut self, idx: usize) {
        if let Some(on) = self.services.get(idx).map(|s| !s.running) {
            self.set_running(idx, on);
        }
    }

    /// Really start or stop one service, logging what actually happened.
    fn set_running(&mut self, idx: usize, on: bool) {
        let Some(s) = self.services.get(idx) else { return };
        let (name, section, version) = (s.name.clone(), s.bin_section, s.version.clone());
        match section {
            None => {
                let mailpit = name.contains("Mailpit");
                let cfg = crate::settings::current();
                let ports = if mailpit { [cfg.mailpit_port, cfg.smtp_port] } else { [cfg.web_port, cfg.https_port] };
                // Our listeners are closed while stopped, so an answer means another program.
                // Our listeners are closed while stopped, so an answer means another program.
                let busy = |p: u16| on && !self.services[idx].running && crate::mysql::port_open(p);
                if busy(ports[0]) {
                    self.log(LogLevel::Error, &format!("{name}: port {} is already in use by another program. Change it in Settings.", ports[0]));
                    return;
                }
                let mut notes = Vec::new();
                if mailpit {
                    if busy(ports[1]) {
                        notes.push(format!("SMTP port {} is taken by another program, so mail can't arrive until it's free", ports[1]));
                    }
                } else if on {
                    let https = !busy(ports[1]);
                    let port80 = cfg.web_port != 80 && !busy(80);
                    crate::server::SERVE_HTTPS.store(https, Ordering::SeqCst);
                    crate::server::SERVE_80.store(port80, Ordering::SeqCst);
                    if !https {
                        notes.push(format!("HTTPS port {} is taken by another program (Laragon's Apache?), so no HTTPS", ports[1]));
                    }
                    if !port80 && cfg.web_port != 80 {
                        notes.push("port 80 is taken by another program, so use :{web} in URLs".replace("{web}", &cfg.web_port.to_string()));
                    }
                }
                let flag = if mailpit { &self.mailpit_running_flag } else { &self.server_running_flag };
                flag.store(on, Ordering::SeqCst);
                self.services[idx].running = on;
                let what = if mailpit {
                    format!("Mailpit (web UI on {}, SMTP on {})", ports[0], ports[1])
                } else if on {
                    let p80 = if crate::server::SERVE_80.load(Ordering::SeqCst) { " and 80" } else { "" };
                    let tls = if crate::server::SERVE_HTTPS.load(Ordering::SeqCst) { format!(", HTTPS on {}", ports[1]) } else { String::new() };
                    format!("Web server (HTTP on {}{p80}{tls})", ports[0])
                } else {
                    "Web server".to_string()
                };
                for n in notes {
                    self.log(LogLevel::Warning, &format!("{name}: {n}."));
                }
                self.log(LogLevel::Success, &format!("{what} {}.", if on { "started" } else { "stopped" }));
            }
            Some("redis") if on => {
                if self.services[idx].available_versions.is_empty() {
                    self.log(LogLevel::Warning, "No Redis installed: use ⚙ Options → Install.");
                    return;
                }
                match self.redis.start(&version, self.redis_port) {
                    Ok(msg) => self.log(LogLevel::Info, &msg),
                    Err(e) => self.log(LogLevel::Error, &format!("Redis: {e}")),
                }
                self.sync_daemons();
            }
            Some("redis") => {
                let msg = self.redis.stop();
                self.log(LogLevel::Info, &format!("Redis: {msg}"));
                self.sync_daemons();
            }
            Some("mysql") if on => {
                if self.services[idx].available_versions.is_empty() {
                    self.log(LogLevel::Warning, "No MySQL / MariaDB installed: use ⚙ Options → Install.");
                    return;
                }
                match self.mysql.start(crate::mycraft_root(), &version, self.mysql_port) {
                    Ok(msg) => self.log(LogLevel::Info, &msg),
                    Err(e) => self.log(LogLevel::Error, &format!("MySQL / MariaDB: {e}")),
                }
                self.sync_daemons();
            }
            Some("mysql") => {
                let msg = self.mysql.stop();
                self.log(LogLevel::Info, &format!("MySQL / MariaDB: {msg}"));
                self.sync_daemons();
            }
            Some("php") => self.log(LogLevel::Info, "PHP needs no starting: each site runs its PHP per request."),
            Some("nodejs") => self.log(LogLevel::Info, "Node.js runs per project: ⚙ Node.js → Node.js Terminal, then `npm run dev`."),
            Some(_) => self.log(LogLevel::Warning, &format!("{name} has no runner in myCraft.")),
        }
        self.all_running = self.services.iter().filter(|s| Self::can_run(s)).all(|s| s.running);
    }

    /// Mirror the MariaDB and Redis process states into their rows; log a crash once.
    pub fn sync_daemons(&mut self) {
        for section in ["mysql", "redis"] {
            let Some(idx) = self.services.iter().position(|s| s.bin_section == Some(section)) else { continue };
            let was = self.services[idx].running;
            let status = if section == "mysql" { self.mysql.status() } else { self.redis.status() };
            let now = matches!(status, crate::mysql::DbStatus::Running | crate::mysql::DbStatus::Starting);
            self.services[idx].running = now;
            if was && !now {
                if let crate::mysql::DbStatus::Failed(e) = &status {
                    let name = self.services[idx].name.clone();
                    self.log(LogLevel::Error, &format!("{name} stopped: {e}"));
                }
            }
            if section == "mysql" {
                self.mysql_status = status;
            } else {
                self.redis_status = status;
            }
            if was != now {
                self.all_running = self.services.iter().filter(|s| Self::can_run(s)).all(|s| s.running);
            }
        }
    }

    /// Stop and start a running service (picks up config changes).
    pub fn restart_service(&mut self, idx: usize) {
        if self.services.get(idx).is_some_and(|s| s.running) {
            self.set_running(idx, false);
            self.set_running(idx, true);
        }
    }

    /// Apply worker-thread results: log lines and the Quick App outcome.
    pub fn drain_background(&mut self) -> bool {
        let notes: Vec<_> = self.bg_rx.try_iter().collect();
        let any = !notes.is_empty();
        for n in notes {
            match n {
                crate::quickapp::Note::Log(level, msg) => self.log(level, &msg),
                crate::quickapp::Note::Done(summary) => {
                    self.quick_busy = false;
                    let title = if summary.starts_with("Failed:") {
                        "Quick App Failed"
                    } else {
                        "Quick App Finished"
                    };
                    crate::notify::send(title, &summary);
                    self.quick_create_status = Some(summary);
                }
                crate::quickapp::Note::Refresh => {
                    self.ssl_trusted = crate::ssl::is_trusted();
                    let (wired, msg) = Self::check_dns_wired(&self.www_root, &self.domain);
                    self.dns_wired = wired;
                    self.dns_check_message = msg;
                }
            }
        }
        any
    }

    /// Save the Settings tab: usr\mycraft.json, the Run key, and a restart of
    /// MySQL / Redis when their port changed (the built-in servers re-bind by
    /// themselves).
    pub fn save_settings(&mut self) {
        if self.domain.trim().is_empty() {
            self.domain = "{name}.go".into();
        }
        let old = crate::settings::current();
        let new = crate::settings::Settings {
            web_port: self.web_port,
            https_port: self.https_port,
            mysql_port: self.mysql_port,
            redis_port: self.redis_port,
            mailpit_port: self.mailpit_port,
            smtp_port: self.smtp_port,
            auto_vhosts: self.auto_vhosts,
            domain: self.domain.clone(),
            close_to_tray: self.close_to_tray,
            notifications: self.notifications,
            db_tool: self.db_tool_preference.clone(),
            terminal: self.terminal_preference.clone(),
        };
        if let Err(e) = crate::settings::save(&new) {
            self.log(LogLevel::Error, &format!("Settings not saved: {e}"));
            self.toast_error(format!("Settings not saved: {e}"));
            return;
        }
        self.log(LogLevel::Success, &format!("Settings saved to {}.", crate::settings::path().display()));
        self.toast_success("Setting successfully applied");
        if old.domain != new.domain {
            self.recheck_admin_and_dns();
            if self.auto_vhosts && self.is_admin && !self.dns_wired {
                self.wire_dns();
            }
        }
        if self.auto_start != crate::settings::autostart_enabled() {
            match crate::settings::set_autostart(self.auto_start) {
                Ok(()) if self.auto_start => self.log(LogLevel::Success, "myCraft will start when you sign in to Windows."),
                Ok(()) => self.log(LogLevel::Success, "myCraft no longer starts with Windows."),
                Err(e) => self.log(LogLevel::Error, &format!("Could not change start-with-Windows: {e}")),
            }
        }
        for s in self.services.iter_mut() {
            match (s.bin_section, s.name.contains("Mailpit")) {
                (None, false) => (s.port, s.aux_port) = (new.web_port, Some(new.https_port)),
                (None, true) => (s.port, s.aux_port) = (new.mailpit_port, Some(new.smtp_port)),
                (Some("mysql"), _) => s.port = new.mysql_port,
                (Some("redis"), _) => s.port = new.redis_port,
                _ => {}
            }
        }
        for (section, changed) in [("mysql", old.mysql_port != new.mysql_port), ("redis", old.redis_port != new.redis_port)] {
            if let Some(idx) = self.services.iter().position(|s| s.bin_section == Some(section) && s.running).filter(|_| changed) {
                self.restart_service(idx);
            }
        }
    }

    /// The config file a service really reads, with its default content.
    pub fn config_file(s: &Service) -> Option<(std::path::PathBuf, &'static str)> {
        match s.bin_section {
            Some("mysql") => Some((crate::mycraft_root().join("conf").join("my.ini"), crate::mysql::DEFAULT_MY_INI)),
            Some("redis") => Some((crate::redis::conf_path(), crate::redis::DEFAULT_CONF)),
            _ => None,
        }
    }

    pub fn open_config(&mut self, idx: usize) {
        let Some(s) = self.services.get(idx) else { return };
        let Some((path, default)) = Self::config_file(s) else { return };
        // The real file when there is one; the default (written on first start) otherwise.
        let content = std::fs::read_to_string(&path).unwrap_or_else(|_| default.to_string());
        self.config_modal = ConfigModal {
            is_open: true,
            title: format!("Configuration Editor: {}", s.name),
            file_path: path.to_string_lossy().to_string(),
            content,
        };
    }

    pub fn templates() -> Vec<QuickTemplate> {
        vec![
            QuickTemplate { id: "wordpress", name: "WordPress", description: "Latest wordpress.org + database + wp-config", category: "CMS", badge: "PHP / MySQL" },
            QuickTemplate { id: "laravel", name: "Laravel", description: "composer create-project + database + .env", category: "Backend", badge: "PHP / Composer" },
            QuickTemplate { id: "nextjs", name: "Next.js", description: "create-next-app (then npm run dev)", category: "Fullstack", badge: "Node.js" },
            QuickTemplate { id: "static", name: "Static HTML", description: "index.html, style.css, app.js", category: "Frontend", badge: "HTML / CSS" },
        ]
    }

    /// Scaffold the selected template into www\<name> on a worker thread.
    pub fn create_project(&mut self) {
        if self.quick_busy {
            return;
        }
        let name = self.new_app_name.trim().to_string();
        let suffix = crate::settings::extract_suffix(&self.domain);
        if !crate::quickapp::valid_name(&name) {
            self.quick_create_status = Some(format!("Use lowercase letters, digits, - and _ (it becomes <name>{suffix} and the database name)."));
            return;
        }
        let templates = Self::templates();
        let t = templates.get(self.selected_template).cloned().unwrap_or(templates[0].clone());
        let kind = match t.id {
            "wordpress" => crate::quickapp::Kind::WordPress,
            "laravel" => crate::quickapp::Kind::Laravel,
            "nextjs" => crate::quickapp::Kind::NextJs,
            _ => crate::quickapp::Kind::Static,
        };
        let dir = std::path::Path::new(&self.www_root).join(&name);
        if std::fs::read_dir(&dir).map(|mut d| d.next().is_some()).unwrap_or(false) {
            self.quick_create_status = Some(format!("{} already exists and isn't empty.", dir.display()));
            return;
        }
        if let Err(e) = std::fs::create_dir_all(&dir) {
            self.log(LogLevel::Error, &format!("Could not create {}: {e}", dir.display()));
            return;
        }
        let sql = if matches!(kind, crate::quickapp::Kind::Laravel | crate::quickapp::Kind::WordPress) { self.mysql.client() } else { None };
        let domain_setting = self.domain.clone();
        let hostname = crate::settings::format_hostname(&domain_setting, &name);
        self.log(LogLevel::Info, &format!("Creating {} project `{name}` in {}… (see progress on Log menu)", t.name, dir.display()));
        self.quick_busy = true;
        self.quick_create_status = Some(format!("Creating {name}… (see progress on Log menu)"));
        let job = crate::quickapp::Job { name, hostname, kind, dir, sql, tx: self.bg_tx.clone() };
        std::thread::spawn(move || job.run());
        // The folder exists now, so its virtual host entry can be added right away.
        if self.auto_vhosts {
            self.wire_dns();
        }
    }

    /// Add the local CA to the user's Root store (Windows asks to confirm).
    pub fn trust_ssl(&mut self) {
        let tx = self.bg_tx.clone();
        self.log(LogLevel::Info, "Adding the myCraft CA to your trusted roots: confirm the Windows prompt.");
        let suffix = crate::settings::extract_suffix(&self.domain);
        std::thread::spawn(move || {
            let (level, msg) = match crate::ssl::trust() {
                Ok(()) => (LogLevel::Success, format!("Trusted {}: https://<project>{suffix} works in Chrome and Edge after a browser restart.", crate::ssl::ca_cert_path().display())),
                Err(e) => (LogLevel::Error, format!("The CA was not trusted: {e}")),
            };
            let _ = tx.send(crate::quickapp::Note::Log(level, msg));
            let _ = tx.send(crate::quickapp::Note::Refresh);
        });
    }

    /// Set a random root password on the running server and copy it to the
    /// clipboard. Works while root still has no password (a fresh install).
    pub fn reset_mysql_root_password(&mut self) -> Option<String> {
        let charset = b"abcdefghijkmnpqrstuvwxyzABCDEFGHJKLMNPQRSTUVWXYZ23456789";
        // Seeded from the clock and a fresh hasher key: fine for a local dev box.
        use std::hash::{BuildHasher, Hasher};
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
        let mut state = h.finish();
        let pwd: String = (0..12)
            .map(|_| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                charset[(state >> 33) as usize % charset.len()] as char
            })
            .collect();
        match self.mysql.set_root_password(&pwd) {
            Ok(()) => {
                let _ = crate::hide_console(std::process::Command::new("cmd.exe").arg("/c").arg(format!("echo|set /p={pwd}| clip"))).spawn();
                self.log(LogLevel::Success, "MySQL root password changed and copied to the clipboard. Put it in your apps' .env (DB_PASSWORD).");
                Some(pwd)
            }
            Err(e) => {
                self.log(LogLevel::Error, &format!("Could not change the root password: {e}"));
                None
            }
        }
    }

    pub fn switch_service_version(&mut self, s_idx: usize, version: &str) {
        let Some(s) = self.services.get_mut(s_idx) else { return };
        let old = std::mem::replace(&mut s.version, version.to_string());
        let (name, running) = (s.name.clone(), s.running);
        self.log(LogLevel::Success, &format!("{name}: switched from {old} to {version}."));
        if running {
            self.set_running(s_idx, false);
            self.set_running(s_idx, true);
        }
    }

    /// Pin a project to a PHP line (`.php-version`), or `None` for automatic.
    pub fn set_project_php(&mut self, project: &str, line: Option<&str>) {
        let dir = std::path::Path::new(&self.www_root).join(project);
        match crate::php_update::set_pin(&dir, line) {
            Ok(()) => match line {
                Some(l) => self.log(LogLevel::Success, &format!("{project} now runs on PHP {l} (.php-version).")),
                None => self.log(LogLevel::Success, &format!("{project} now picks its PHP from composer.json.")),
            },
            Err(e) => self.log(LogLevel::Error, &format!("Could not set PHP for {project}: {e}")),
        }
    }

    pub fn check_is_admin() -> bool {
        #[cfg(windows)]
        {
            // On Windows, opening System32/drivers/etc/hosts in write/append mode strictly requires elevated token
            match std::fs::OpenOptions::new()
                .write(true)
                .append(true)
                .open("C:\\Windows\\System32\\drivers\\etc\\hosts")
            {
                Ok(_) => true,
                Err(_) => false,
            }
        }
        #[cfg(not(windows))]
        {
            true
        }
    }

    /// Project folders in www that are served as virtual hosts.
    pub fn project_names(www_root: &str) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(www_root)
            .map(|rd| {
                rd.flatten()
                    .filter(|e| e.path().is_dir())
                    .map(|e| e.file_name().to_string_lossy().to_string())
                    .filter(|n| !n.starts_with('.') && !n.starts_with('_') && !n.contains(' '))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Projects whose domain has no `127.0.0.1` line in the hosts file.
    pub fn missing_hosts(www_root: &str, pattern: &str) -> Result<Vec<String>, String> {
        let hosts = std::fs::read_to_string(HOSTS).map_err(|e| format!("cannot read the hosts file: {e}"))?;
        let mapped: std::collections::HashSet<String> = hosts
            .lines()
            .map(|l| l.split('#').next().unwrap_or(""))
            .filter_map(|l| {
                let mut parts = l.split_whitespace();
                (parts.next()? == "127.0.0.1").then(|| parts.map(str::to_ascii_lowercase).collect::<Vec<_>>())
            })
            .flatten()
            .collect();
        Ok(Self::project_names(www_root)
            .into_iter()
            .filter(|n| {
                let host = crate::settings::format_hostname(pattern, n).to_ascii_lowercase();
                !mapped.contains(&host)
            })
            .collect())
    }

    pub fn check_dns_wired(www_root: &str, pattern: &str) -> (bool, String) {
        let suffix = crate::settings::extract_suffix(pattern);
        match Self::missing_hosts(www_root, pattern) {
            Ok(missing) if missing.is_empty() => (true, format!("DNS: every project's <name>{suffix} maps to 127.0.0.1 in hosts.")),
            Ok(missing) => {
                let formatted = missing.iter().map(|n| crate::settings::format_hostname(pattern, n)).collect::<Vec<_>>();
                (false, format!("DNS: no hosts entry for {}", formatted.join(", ")))
            }
            Err(e) => (false, format!("DNS: {e}")),
        }
    }

    pub fn recheck_admin_and_dns(&mut self) {
        self.is_admin = Self::check_is_admin();
        let (wired, msg) = Self::check_dns_wired(&self.www_root, &self.domain);
        self.dns_wired = wired;
        self.dns_check_message = msg.clone();
        self.log(if wired { LogLevel::Success } else { LogLevel::Warning }, &msg);
    }

    /// Add `127.0.0.1 <hostname>` for every project missing from hosts:
    /// directly when elevated, otherwise through a UAC-elevated PowerShell.
    pub fn wire_dns(&mut self) {
        let missing = match Self::missing_hosts(&self.www_root, &self.domain) {
            Ok(m) => m,
            Err(e) => {
                self.log(LogLevel::Error, &format!("DNS: {e}"));
                return;
            }
        };
        if missing.is_empty() {
            self.recheck_admin_and_dns();
            return;
        }
        let names = missing.iter().map(|n| crate::settings::format_hostname(&self.domain, n).to_ascii_lowercase()).collect::<Vec<_>>();
        if self.is_admin {
            use std::io::Write;
            let lines: String = names.iter().map(|n| format!("\r\n127.0.0.1 {n}")).collect();
            let result = std::fs::OpenOptions::new().append(true).open(HOSTS).and_then(|mut f| f.write_all(lines.as_bytes()));
            match result {
                Ok(()) => self.log(LogLevel::Success, &format!("Added {} to the hosts file.", names.join(", "))),
                Err(e) => self.log(LogLevel::Error, &format!("Could not write the hosts file: {e}")),
            }
            self.recheck_admin_and_dns();
            return;
        }
        // Not elevated: Windows asks (UAC) to run the append; check the result after.
        let entries = names.iter().map(|n| format!("'127.0.0.1 {n}'")).collect::<Vec<_>>().join(",");
        let inner = format!("Add-Content -LiteralPath '{HOSTS}' -Value (@('') + @({entries}))");
        let mut cmd = std::process::Command::new("powershell.exe");
        cmd.args(["-NoProfile", "-Command"]).arg(format!(
            "try {{ $p = Start-Process powershell -Verb RunAs -WindowStyle Hidden -Wait -PassThru -ArgumentList '-NoProfile','-Command',\"{inner}\"; exit $p.ExitCode }} catch {{ exit 1 }}"
        ));
        self.log(LogLevel::Info, &format!("Asking for administrator rights to add {} to hosts…", names.join(", ")));
        let tx = self.bg_tx.clone();
        let www = self.www_root.clone();
        let domain = self.domain.clone();
        std::thread::spawn(move || {
            let _ = crate::hide_console(&mut cmd).status();
            let (level, msg) = match Self::missing_hosts(&www, &domain) {
                Ok(m) if m.is_empty() => (LogLevel::Success, "Hosts file updated: every project resolves to 127.0.0.1.".to_string()),
                Ok(m) => {
                    let formatted = m.iter().map(|n| crate::settings::format_hostname(&domain, n)).collect::<Vec<_>>();
                    (LogLevel::Error, format!("Hosts file not updated (UAC declined?): {} still missing.", formatted.join(", ")))
                }
                Err(e) => (LogLevel::Error, format!("DNS: {e}")),
            };
            let _ = tx.send(crate::quickapp::Note::Log(level, msg));
            let _ = tx.send(crate::quickapp::Note::Refresh);
        });
    }

    pub fn restart_as_admin(&mut self) {
        if let Ok(current_exe) = std::env::current_exe() {
            let exe_str = current_exe.to_string_lossy().to_string();
            self.log(LogLevel::Info, "Restarting Mycraft with Administrator privileges (UAC prompt)...");
            crate::single_instance::release();
            let _ = crate::hide_console(
                std::process::Command::new("powershell.exe")
                    .arg("-Command")
                    .arg(format!("Start-Process -FilePath '{}' -Verb RunAs", exe_str)),
            )
            .spawn();
            std::process::exit(0);
        }
    }
}
