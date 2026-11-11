use eframe::egui::{self, Color32, RichText, Stroke, Vec2};
use std::path::PathBuf;
use std::process::Command;

use crate::state::{ActiveTab, MyCraftState, LogLevel};

pub struct MyCraftApp {
    pub state: MyCraftState,
    pub screenshot_target: Option<PathBuf>,
    pub frame_count: usize,
    /// Set by Menu → Exit so the next close request quits instead of minimizing.
    pub quitting: bool,
    #[cfg(windows)]
    tray: Option<crate::tray::Tray>,
    #[cfg(windows)]
    hwnd: isize,
}

impl MyCraftApp {
    pub fn new(screenshot_target: Option<PathBuf>) -> Self {
        let state = MyCraftState::default();
        Self {
            state,
            screenshot_target,
            frame_count: 0,
            quitting: false,
            #[cfg(windows)]
            tray: None,
            #[cfg(windows)]
            hwnd: 0,
        }
    }

    #[cfg(windows)]
    pub fn attach_tray(&mut self, cc: &eframe::CreationContext<'_>) {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        if let Ok(RawWindowHandle::Win32(h)) = cc.window_handle().map(|h| h.as_raw()) {
            self.hwnd = h.hwnd.get();
        }
        self.tray = crate::tray::Tray::new(&cc.egui_ctx, &self.state);
        if self.tray.is_none() {
            self.state.log(LogLevel::Warning, "System tray icon unavailable; closing the window will quit.");
        }
    }

    /// Run commands clicked in the tray menu, then refresh its check marks.
    #[cfg(windows)]
    fn handle_tray(&mut self, ctx: &egui::Context) {
        use crate::tray::{show_window, TrayCmd};
        let Some(tray) = &self.tray else { return };
        for cmd in tray.poll() {
            match cmd {
                TrayCmd::Show => show_window(self.hwnd),
                TrayCmd::ToggleAll => {
                    if self.state.all_running {
                        self.state.set_loading("stopping_all", 900);
                        self.state.stop_all();
                    } else {
                        self.state.set_loading("starting_all", 900);
                        self.state.start_all();
                    }
                }
                TrayCmd::Service(i) => self.state.toggle_service(i),
                TrayCmd::Web => {
                    let url = format!("http://localhost:{}", self.state.web_port);
                    self.open_url(&url);
                }
                TrayCmd::Database => self.open_database(),
                TrayCmd::Terminal => self.open_terminal(),
                TrayCmd::Root => self.open_root_folder(),
                TrayCmd::Settings => {
                    self.state.active_tab = ActiveTab::Settings;
                    show_window(self.hwnd);
                }
                TrayCmd::Exit => {
                    self.quitting = true;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        if let Some(tray) = &self.tray {
            tray.sync(&self.state);
        }
    }

    /// Whether the close button should hide to the tray instead of quitting.
    fn hides_to_tray(&self) -> bool {
        #[cfg(windows)]
        return self.state.close_to_tray && self.tray.is_some() && self.hwnd != 0 && !self.quitting;
        #[cfg(not(windows))]
        false
    }

    fn open_url(&mut self, url: &str) {
        self.state.log(LogLevel::Info, &format!("Opening URL: {}", url));
        let _ = webbrowser::open(url);
    }

    fn open_database(&mut self) {
        if let Some(idx) = self.state.services.iter().position(|s| s.bin_section == Some("mysql") && !s.running && !s.available_versions.is_empty()) {
            self.state.toggle_service(idx);
        }
        let preference = self.state.db_tool_preference.clone();
        self.state.log(LogLevel::Info, &format!("Opening database manager (Tool: {})...", preference));

        let mut launched = false;

        if preference == "HeidiSQL" || preference.is_empty() {
            let bin = crate::mycraft_root().join("bin").join("heidisql");
            let found = std::iter::once(bin.join("heidisql.exe"))
                .chain(crate::packages::installed("heidisql").into_iter().map(|d| bin.join(d).join("heidisql.exe")))
                .chain(["C:\\Program Files\\HeidiSQL\\heidisql.exe", "C:\\Program Files (x86)\\HeidiSQL\\heidisql.exe"].map(PathBuf::from))
                .find(|p| p.is_file());
            match found {
                Some(exe) => {
                    // HeidiSQL's command line opens a session straight to our server.
                    let mut cmd = Command::new(&exe);
                    cmd.args(["-n=0", "-h=127.0.0.1", "-u=root", &format!("-P={}", self.state.mysql_port), "-d=myCraft"]);
                    if let Some(pw) = self.state.mysql.root_password() {
                        cmd.arg(format!("-p={pw}"));
                    }
                    match cmd.spawn() {
                        Ok(_) => {
                            self.state.log(LogLevel::Success, &format!("Opened HeidiSQL ({}) on 127.0.0.1:{}.", exe.display(), self.state.mysql_port));
                            launched = true;
                        }
                        Err(e) => self.state.log(LogLevel::Error, &format!("Could not start {}: {e}", exe.display())),
                    }
                }
                None => {
                    if let Some(p) = crate::packages::load().into_iter().find(|p| p.section == "heidisql") {
                        self.state.installer.install(p);
                        self.state.log(LogLevel::Info, "HeidiSQL isn't installed: downloading it now. Click Database again when it finishes.");
                        return;
                    }
                }
            }
        } else if preference == "DBeaver" {
            let paths = [
                "C:\\Program Files\\DBeaver\\dbeaver.exe",
                "C:\\Program Files (x86)\\DBeaver\\dbeaver.exe",
            ];
            for p in &paths {
                if std::path::Path::new(p).exists() {
                    if let Ok(_) = Command::new(p).spawn() {
                        self.state.log(LogLevel::Success, &format!("Launched DBeaver ({})", p));
                        launched = true;
                        break;
                    }
                }
            }
        } else if preference == "Navicat" {
            let paths = [
                "C:\\Program Files\\PremiumSoft\\Navicat Premium 16\\navicat.exe",
                "C:\\Program Files\\PremiumSoft\\Navicat Premium 15\\navicat.exe",
                "C:\\Program Files\\PremiumSoft\\Navicat Premium\\navicat.exe",
                "C:\\Program Files (x86)\\PremiumSoft\\Navicat Premium\\navicat.exe",
            ];
            for p in &paths {
                if std::path::Path::new(p).exists() {
                    if let Ok(_) = Command::new(p).spawn() {
                        self.state.log(LogLevel::Success, &format!("Launched Navicat ({})", p));
                        launched = true;
                        break;
                    }
                }
            }
        } else if preference == "MySQL Workbench" {
            let paths = [
                "C:\\Program Files\\MySQL\\MySQL Workbench 8.0 CE\\MySQLWorkbench.exe",
                "C:\\Program Files\\MySQL\\MySQL Workbench\\MySQLWorkbench.exe",
            ];
            for p in &paths {
                if std::path::Path::new(p).exists() {
                    if let Ok(_) = Command::new(p).spawn() {
                        self.state.log(LogLevel::Success, &format!("Launched MySQL Workbench ({})", p));
                        launched = true;
                        break;
                    }
                }
            }
        }

        // Fallback to PHP Adminer
        if !launched {
            if preference != "PHP Adminer (Web)" {
                self.state.log(LogLevel::Warning, &format!("{} not found; opening Adminer instead.", preference));
            } else {
                self.state.log(LogLevel::Info, "Opening PHP Adminer web database manager...");
            }
            if !self.state.server_running_flag.load(std::sync::atomic::Ordering::SeqCst) {
                self.state.toggle_service(0);
            }
            let adminer_url = format!("http://localhost:{}/adminer/", self.state.web_port);
            self.open_url(&adminer_url);
        }
    }

    fn open_terminal(&mut self) {
        let www_path = self.state.www_root.clone();
        let preference = self.state.terminal_preference.clone();
        self.state.log(LogLevel::Info, &format!("Opening terminal (Preference: {}) at {:?}", preference, www_path));

        let mut launched = false;

        if preference == "Cmder" || preference.is_empty() {
            let root = self.state.root_dir();
            let cmder_paths = [
                format!("{}\\bin\\cmder\\Cmder.exe", root),
                "C:\\tools\\cmder\\Cmder.exe".to_string(),
            ];
            for p in &cmder_paths {
                if std::path::Path::new(p).exists() {
                    if let Ok(_) = Command::new(p)
                        .arg("/START")
                        .arg(&www_path)
                        .spawn()
                    {
                        self.state.log(LogLevel::Success, &format!("Launched Cmder ({})", p));
                        launched = true;
                        break;
                    }
                }
            }
        } else if preference == "Windows Terminal" {
            if let Ok(_) = Command::new("wt.exe")
                .arg("-d")
                .arg(&www_path)
                .spawn()
            {
                self.state.log(LogLevel::Success, "Launched Windows Terminal (wt.exe)");
                launched = true;
            }
        } else if preference == "Git Bash" {
            let root = self.state.root_dir();
            let bash_paths = [
                format!("{}\\bin\\git\\git-bash.exe", root),
                "C:\\Program Files\\Git\\git-bash.exe".to_string(),
            ];
            for p in &bash_paths {
                if std::path::Path::new(p).exists() {
                    if let Ok(_) = Command::new(p)
                        .arg(format!("--cd={}", www_path))
                        .spawn()
                    {
                        self.state.log(LogLevel::Success, &format!("Launched Git Bash ({})", p));
                        launched = true;
                        break;
                    }
                }
            }
        } else if preference == "Command Prompt" {
            if let Ok(_) = crate::hide_console(&mut Command::new("cmd.exe"))
                .arg("/c")
                .arg("start")
                .arg("cmd.exe")
                .arg("/K")
                .arg(format!("cd /d \"{}\"", www_path))
                .spawn()
            {
                self.state.log(LogLevel::Success, "Launched Command Prompt");
                launched = true;
            }
        } else if preference == "PowerShell" {
            if let Ok(_) = crate::hide_console(&mut Command::new("cmd.exe"))
                .arg("/c")
                .arg("start")
                .arg("powershell.exe")
                .arg("-NoExit")
                .arg("-Command")
                .arg(format!("Set-Location -LiteralPath '{}'", www_path))
                .spawn()
            {
                self.state.log(LogLevel::Success, "Launched PowerShell");
                launched = true;
            }
        }

        // Fallback to PowerShell
        if !launched {
            let _ = crate::hide_console(&mut Command::new("cmd.exe"))
                .arg("/c")
                .arg("start")
                .arg("powershell.exe")
                .arg("-NoExit")
                .arg("-Command")
                .arg(format!("Set-Location -LiteralPath '{}'", www_path))
                .spawn();
            self.state.log(LogLevel::Success, "Launched PowerShell (fallback)");
        }
    }

    /// A console in www with `path_prefix` first on PATH, running `run` (or a shell).
    fn open_console(&mut self, title: &str, path_prefix: &std::path::Path, run: Option<(PathBuf, Vec<String>)>) {
        let mut cmd = Command::new("cmd.exe");
        cmd.current_dir(&self.state.www_root)
            .env("PATH", format!("{};{}", path_prefix.display(), std::env::var("PATH").unwrap_or_default()))
            // `start` takes the first quoted argument as the window title.
            .args(["/c", "start", &format!("{title} ")]);
        match run {
            Some((exe, args)) => cmd.arg(exe).args(args),
            None => cmd.args(["cmd.exe", "/K"]),
        };
        match crate::hide_console(&mut cmd).spawn() {
            Ok(_) => self.state.log(LogLevel::Success, &format!("Opened {title} ({}).", path_prefix.display())),
            Err(e) => self.state.log(LogLevel::Error, &format!("Could not open {title}: {e}")),
        }
    }

    /// Open a log file in Notepad (creating it empty so Notepad doesn't ask).
    fn open_log(&mut self, path: PathBuf) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let _ = std::fs::OpenOptions::new().create(true).append(true).open(&path);
        let _ = Command::new("notepad.exe").arg(&path).spawn();
    }

    fn open_root_folder(&mut self) {
        let www_path = self.state.www_root.clone();
        self.state.log(LogLevel::Info, &format!("Opening explorer at {:?}", www_path));
        let _ = Command::new("explorer.exe")
            .arg(&www_path)
            .spawn();
    }
}

impl eframe::App for MyCraftApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Screenshot support for automated testing
        if let Some(target) = &self.screenshot_target {
            self.frame_count += 1;
            if self.frame_count < 120 {
                ctx.request_repaint();
            } else if self.frame_count == 120 {
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            }

            ctx.input(|i| {
                for ev in &i.raw.events {
                    if let egui::Event::Screenshot { image, .. } = ev {
                        let width = image.width() as u32;
                        let height = image.height() as u32;
                        let raw = image.as_raw();
                        if let Some(parent) = target.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let _ = image::save_buffer(
                            target,
                            raw,
                            width,
                            height,
                            image::ExtendedColorType::Rgba8,
                        );
                        std::process::exit(0);
                    }
                }
            });
        }

        #[cfg(windows)]
        self.handle_tray(ctx);

        // Track MariaDB / Redis (crash, slow start) and worker results even when idle.
        self.state.sync_daemons();
        if self.state.drain_background() {
            ctx.request_repaint();
        }
        if self.state.mysql.is_ours() || self.state.redis.is_ours() || self.state.quick_busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(500));
        }

        if ctx.input(|i| i.viewport().close_requested()) {
            if self.hides_to_tray() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                #[cfg(windows)]
                crate::tray::hide_window(self.hwnd);
            } else {
                self.state.shutdown();
            }
        }

        // Cleanup expired button loading cooldowns and request repaint while active
        let now = std::time::Instant::now();
        self.state.button_loading.retain(|_, until| *until > now);
        if !self.state.button_loading.is_empty() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }

        // Custom dark slate styling
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = Color32::from_rgb(17, 24, 39); // Slate 900
        visuals.window_fill = Color32::from_rgb(24, 33, 47);
        visuals.widgets.noninteractive.bg_fill = Color32::from_rgb(31, 41, 55); // Slate 800
        visuals.widgets.inactive.bg_fill = Color32::from_rgb(31, 41, 55);
        visuals.widgets.hovered.bg_fill = Color32::from_rgb(55, 65, 81);
        visuals.widgets.active.bg_fill = Color32::from_rgb(75, 85, 99);
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, Color32::from_rgb(226, 232, 240));
        visuals.selection.bg_fill = Color32::from_rgb(37, 99, 235); // Blue 600
        ctx.set_visuals(visuals);

        // Top Header
        egui::TopBottomPanel::top("top_header")
            .frame(egui::Frame::none()
                .fill(Color32::from_rgb(15, 23, 42))
                .inner_margin(egui::Margin::symmetric(14.0, 9.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // Menu button
                    let menu_btn = ui.button(RichText::new("☰ Menu").strong().color(Color32::from_rgb(241, 245, 249)));
                    if menu_btn.clicked() {
                        self.state.menu_open = !self.state.menu_open;
                    }

                    ui.add_space(8.0);
                    ui.heading(RichText::new("MYCRAFT").strong().color(Color32::from_rgb(56, 189, 248)));
                    ui.label(RichText::new(env!("CARGO_PKG_VERSION")).color(Color32::from_rgb(148, 163, 184)));

                    ui.add_space(6.0);
                    let feat_btn = ui.small_button(RichText::new("🔍 Features").size(10.5).color(Color32::from_rgb(147, 197, 253)));
                    if feat_btn.clicked() {
                        self.state.features_modal_open = true;
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Short labels so the header fits the 540 px window; details on hover.
                        let (text, color, tip) = if self.state.all_running {
                            ("ACTIVE", Color32::from_rgb(16, 185, 129), "All startable services are running.")
                        } else {
                            ("STOPPED", Color32::from_rgb(148, 163, 184), "Not every startable service is running.")
                        };
                        ui.label(RichText::new(text).size(10.0).color(color).strong()).on_hover_text(tip);

                        let color = if self.state.dns_wired { Color32::from_rgb(16, 185, 129) } else { Color32::from_rgb(239, 68, 68) };
                        ui.label(RichText::new("⚫ DNS").size(10.0).color(color).strong()).on_hover_text(&self.state.dns_check_message);

                        let (text, color, tip) = if self.state.is_admin {
                            ("🛡 ADMIN", Color32::from_rgb(56, 189, 248), "Running as administrator: hosts can be edited directly.")
                        } else {
                            ("⚠ USER", Color32::from_rgb(251, 191, 36), "Not running as administrator: editing hosts asks for UAC.")
                        };
                        ui.label(RichText::new(text).size(10.0).color(color).strong()).on_hover_text(tip);
                    });
                });

                // Menu Dropdown overlay
                if self.state.menu_open {
                    ui.separator();
                    ui.horizontal_wrapped(|ui| {
                        if ui.button("📁 WWW Root").clicked() {
                            self.open_root_folder();
                            self.state.menu_open = false;
                        }
                        if ui.button("⚡ Quick App").clicked() {
                            self.state.active_tab = ActiveTab::QuickApp;
                            self.state.menu_open = false;
                        }
                        if ui.button("🌐 Virtual Hosts").clicked() {
                            self.state.active_tab = ActiveTab::VirtualHosts;
                            self.state.menu_open = false;
                        }
                        if ui.button("✨ AI Assistant").clicked() {
                            self.state.active_tab = ActiveTab::Ai;
                            self.state.menu_open = false;
                        }
                        if ui.button("🗄 Database").clicked() {
                            self.open_database();
                            self.state.menu_open = false;
                        }
                        if ui.button("💻 Terminal").clicked() {
                            self.open_terminal();
                            self.state.menu_open = false;
                        }
                        if ui.button("⚙ Settings").clicked() {
                            self.state.active_tab = ActiveTab::Settings;
                            self.state.menu_open = false;
                        }
                        if ui.button("🔍 Features Overview").clicked() {
                            self.state.features_modal_open = true;
                            self.state.menu_open = false;
                        }
                        if ui.button("❌ Close Menu").clicked() {
                            self.state.menu_open = false;
                        }
                        // Really quit, even with close-to-taskbar on; services stop first.
                        if ui.button("✖ Exit").clicked() {
                            self.quitting = true;
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });

                    ui.add_space(3.0);
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new("Fast Service Options:").size(11.0).color(Color32::from_rgb(148, 163, 184)));
                        if ui.button("⚙ PHP").clicked() {
                            self.state.service_options_idx = Some(2);
                            self.state.menu_open = false;
                        }
                        if ui.button("⚙ MySQL").clicked() {
                            self.state.service_options_idx = Some(1);
                            self.state.menu_open = false;
                        }
                        if ui.button("⚙ Web Server").clicked() {
                            self.state.service_options_idx = Some(0);
                            self.state.menu_open = false;
                        }
                        if ui.button("⚙ Node.js").clicked() {
                            self.state.service_options_idx = Some(3);
                            self.state.menu_open = false;
                        }
                        if ui.button("⚙ Redis").clicked() {
                            self.state.service_options_idx = Some(4);
                            self.state.menu_open = false;
                        }
                    });
                }
            });

        // Tab Navigation Bar
        egui::TopBottomPanel::top("nav_tabs")
            .frame(egui::Frame::none()
                .fill(Color32::from_rgb(24, 33, 47))
                .inner_margin(egui::Margin::symmetric(14.0, 5.0)))
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    let tabs = [
                        (ActiveTab::Dashboard, "📊 Dashboard"),
                        (ActiveTab::QuickApp, "⚡ Quick App"),
                        (ActiveTab::VirtualHosts, "🌐 Virtual Hosts"),
                        (ActiveTab::Ai, "✨ AI"),
                        (ActiveTab::Logs, "📜 Log"),
                        (ActiveTab::Settings, "⚙ Settings"),
                    ];

                    for (tab, label) in tabs {
                        let is_active = self.state.active_tab == tab;
                        let text = if is_active {
                            RichText::new(label).strong().color(Color32::from_rgb(56, 189, 248))
                        } else {
                            RichText::new(label).color(Color32::from_rgb(203, 213, 225))
                        };

                        if ui.selectable_label(is_active, text).clicked() {
                            self.state.active_tab = tab;
                        }
                    }
                });
            });

        // Bottom Action Bar (Classic Laragon 5-button bar)
        egui::TopBottomPanel::bottom("bottom_bar")
            .frame(egui::Frame::none()
                .fill(Color32::from_rgb(15, 23, 42))
                .inner_margin(egui::Margin::symmetric(14.0, 9.0)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    // Start All / Stop All primary toggle
                    let starting_all = self.state.is_loading("starting_all");
                    let stopping_all = self.state.is_loading("stopping_all");

                    if starting_all || stopping_all {
                        let (bg, label, text_color) = if starting_all {
                            (Color32::from_rgb(16, 185, 129), "Starting…", Color32::from_rgb(15, 23, 42))
                        } else {
                            (Color32::from_rgb(225, 29, 72), "Stopping…", Color32::WHITE)
                        };

                        egui::Frame::none()
                            .fill(bg)
                            .rounding(4.0)
                            .inner_margin(egui::Margin::symmetric(10.0, 5.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spinner();
                                    ui.add_space(2.0);
                                    ui.label(RichText::new(label).strong().size(13.0).color(text_color));
                                });
                            });
                    } else if self.state.all_running {
                        let stop_btn = egui::Button::new(
                            RichText::new("■  Stop All")
                                .strong()
                                .size(13.0)
                                .color(Color32::WHITE),
                        )
                        .fill(Color32::from_rgb(225, 29, 72)) // Crimson Rose 600
                        .min_size(Vec2::new(105.0, 30.0));

                        if ui.add(stop_btn).clicked() {
                            self.state.set_loading("stopping_all", 900);
                            self.state.stop_all();
                        }
                    } else {
                        let start_btn = egui::Button::new(
                            RichText::new("▶  Start All")
                                .strong()
                                .size(13.0)
                                .color(Color32::from_rgb(15, 23, 42)),
                        )
                        .fill(Color32::from_rgb(16, 185, 129)) // Emerald 500
                        .min_size(Vec2::new(105.0, 30.0));

                        if ui.add(start_btn).clicked() {
                            self.state.set_loading("starting_all", 900);
                            self.state.start_all();
                        }
                    }

                    ui.add_space(8.0);

                    // Web Button
                    let loading_web = self.state.is_loading("btn_web");
                    let web_text = if loading_web { "⏳ Opening…" } else { "🌐 Web" };
                    let web_btn = egui::Button::new(RichText::new(web_text).size(12.0))
                        .min_size(Vec2::new(72.0, 30.0));
                    if ui.add_enabled(!loading_web, web_btn).clicked() {
                        self.state.set_loading("btn_web", 600);
                        let url = format!("http://localhost:{}", self.state.web_port);
                        self.open_url(&url);
                    }

                    // Database Button
                    let loading_db = self.state.is_loading("btn_db");
                    let db_text = if loading_db { "⏳ Launching…" } else { "🗄 Database" };
                    let db_btn = egui::Button::new(RichText::new(db_text).size(12.0))
                        .min_size(Vec2::new(84.0, 30.0));
                    if ui.add_enabled(!loading_db, db_btn).clicked() {
                        self.state.set_loading("btn_db", 600);
                        self.open_database();
                    }

                    // Terminal Button
                    let loading_term = self.state.is_loading("btn_term");
                    let term_text = if loading_term { "⏳ Opening…" } else { "💻 Terminal" };
                    let term_btn = egui::Button::new(RichText::new(term_text).size(12.0))
                        .min_size(Vec2::new(84.0, 30.0));
                    if ui.add_enabled(!loading_term, term_btn).clicked() {
                        self.state.set_loading("btn_term", 600);
                        self.open_terminal();
                    }

                    // Root Button
                    let loading_root = self.state.is_loading("btn_root");
                    let root_text = if loading_root { "⏳ Opening…" } else { "📁 Root" };
                    let root_btn = egui::Button::new(RichText::new(root_text).size(12.0))
                        .min_size(Vec2::new(72.0, 30.0));
                    if ui.add_enabled(!loading_root, root_btn).clicked() {
                        self.state.set_loading("btn_root", 600);
                        self.open_root_folder();
                    }
                });
            });

        // Main Center Content Area
        egui::CentralPanel::default().show(ctx, |ui| {
            if !self.state.is_admin || !self.state.dns_wired {
                egui::Frame::none()
                    .fill(Color32::from_rgb(45, 26, 15)) // Dark amber
                    .stroke(Stroke::new(1.0_f32, Color32::from_rgb(217, 119, 6)))
                    .rounding(4.0)
                    .inner_margin(egui::Margin::symmetric(8.0, 3.0))
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let suffix = crate::settings::extract_suffix(&self.state.domain);
                            let text = if self.state.dns_wired { "⚠ NOT ADMIN".to_string() } else { format!("⚠ SOME *{suffix} NAMES AREN'T IN HOSTS") };
                            ui.label(RichText::new(&text).strong().color(Color32::from_rgb(251, 191, 36)).size(10.5))
                                .on_hover_text(&self.state.dns_check_message);
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                let loading_recheck = self.state.is_loading("header_recheck");
                                let recheck_text = if loading_recheck { "⏳ Checking…" } else { "🔄 Recheck" };
                                if ui.add_enabled(!loading_recheck, egui::Button::new(RichText::new(recheck_text).size(10.0)).small()).clicked() {
                                    self.state.set_loading("header_recheck", 600);
                                    self.state.recheck_admin_and_dns();
                                }

                                let loading_wire = self.state.is_loading("header_wire_dns");
                                let wire_text = if loading_wire { "⏳ Wiring…" } else { "⚡ Wire DNS" };
                                if ui.add_enabled(!loading_wire, egui::Button::new(RichText::new(wire_text).size(10.0)).small()).clicked() {
                                    self.state.set_loading("header_wire_dns", 1000);
                                    self.state.wire_dns();
                                }

                                if !self.state.is_admin {
                                    let loading_admin = self.state.is_loading("header_restart_admin");
                                    let admin_text = if loading_admin { "⏳ Restarting…" } else { "🛡 Restart as Admin" };
                                    if ui.add_enabled(!loading_admin, egui::Button::new(RichText::new(admin_text).size(10.0)).small()).clicked() {
                                        self.state.set_loading("header_restart_admin", 1000);
                                        self.state.restart_as_admin();
                                    }
                                }
                            });
                        });
                    });
                ui.add_space(2.0);
            }

            match self.state.active_tab {
                ActiveTab::Dashboard => self.render_dashboard(ui),
                ActiveTab::QuickApp => self.render_quick_app(ui),
                ActiveTab::VirtualHosts => self.render_virtual_hosts(ui),
                ActiveTab::Ai => crate::ai_tab::show(&mut self.state, ui),
                ActiveTab::Logs => self.render_logs(ui),
                ActiveTab::Settings => self.render_settings(ui),
            }
        });

        // Config Modal Popup
        if self.state.config_modal.is_open {
            egui::Window::new(&self.state.config_modal.title)
                .collapsible(false)
                .resizable(true)
                .default_size([500.0, 320.0])
                .show(ctx, |ui| {
                    ui.label(RichText::new(&self.state.config_modal.file_path).monospace().color(Color32::from_rgb(148, 163, 184)));
                    ui.separator();
                    ui.add(
                        egui::TextEdit::multiline(&mut self.state.config_modal.content)
                            .font(egui::TextStyle::Monospace)
                            .desired_rows(12)
                            .desired_width(f32::INFINITY),
                    );
                    ui.separator();
                    ui.horizontal(|ui| {
                        let loading = self.state.is_loading("save_config");
                        let btn_text = if loading { "⏳ Saving…" } else { "Save Configuration" };
                        if ui.add_enabled(!loading, egui::Button::new(btn_text)).clicked() {
                            self.state.set_loading("save_config", 600);
                            let m = &self.state.config_modal;
                            let (path, content) = (m.file_path.clone(), m.content.clone());
                            let result = std::path::Path::new(&path)
                                .parent()
                                .map_or(Ok(()), std::fs::create_dir_all)
                                .and_then(|_| std::fs::write(&path, content));
                            match result {
                                Ok(()) => {
                                    self.state.log(LogLevel::Success, &format!("Saved {path}. Restart the service to apply it."));
                                    self.state.toast_success("Configuration successfully saved");
                                    self.state.config_modal.is_open = false;
                                }
                                Err(e) => {
                                    self.state.log(LogLevel::Error, &format!("Could not save {path}: {e}"));
                                    self.state.toast_error(format!("Could not save configuration: {e}"));
                                }
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            self.state.config_modal.is_open = false;
                        }
                    });
                });
        }

        // Service Options Modal Popup
        if let Some(s_idx) = self.state.service_options_idx {
            self.render_service_options_modal(ctx, s_idx);
        }

        // Features Overview Modal Popup
        if self.state.features_modal_open {
            self.render_features_modal(ctx);
        }

        // Floating toast notifications
        self.render_toasts(ctx);
    }
}

impl MyCraftApp {
    fn render_dashboard(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("Full Stack Services").strong().size(13.0));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(RichText::new(format!("Root: {}", self.state.www_root)).size(10.5).color(Color32::from_rgb(148, 163, 184)));
            });
        });

        ui.add_space(2.0);

        egui::Frame::none()
            .fill(Color32::from_rgb(24, 33, 47))
            .stroke(Stroke::new(1.0_f32, Color32::from_rgb(45, 55, 72)))
            .rounding(6.0)
            .inner_margin(egui::Margin::symmetric(10.0, 3.0))
            .show(ui, |ui| {
                // Table header (fluid layout)
                let port_w = 60.0;
                let status_w = 68.0;
                let actions_w = 205.0;
                let avail_w = ui.available_width();
                let service_w = (avail_w - port_w - status_w - actions_w - 24.0).max(115.0);

                ui.horizontal(|ui| {
                    ui.add_sized([service_w, 15.0], egui::Label::new(RichText::new("Service").strong().color(Color32::from_rgb(148, 163, 184))));
                    ui.add_sized([port_w, 15.0], egui::Label::new(RichText::new("Port").strong().color(Color32::from_rgb(148, 163, 184))));
                    ui.add_sized([status_w, 15.0], egui::Label::new(RichText::new("Status").strong().color(Color32::from_rgb(148, 163, 184))));
                    ui.add_sized([actions_w, 15.0], egui::Label::new(RichText::new("Actions").strong().color(Color32::from_rgb(148, 163, 184))));
                });

                ui.separator();

                let mut toggle_idx = None;
                let mut reload_idx = None;
                let mut config_idx = None;
                let mut options_idx = None;

                // Cheap (a few read_dirs) and keeps rows true after installs.
                MyCraftState::detect_versions(&mut self.state.services);

                for (idx, s) in self.state.services.iter().enumerate() {
                    let missing = s.bin_section.is_some() && s.available_versions.is_empty();
                    ui.horizontal(|ui| {
                        // Name and version
                        ui.add_sized([service_w, 18.0], |ui: &mut egui::Ui| {
                            ui.vertical(|ui| {
                                ui.label(RichText::new(&s.name).strong().size(11.0));
                                ui.label(RichText::new(&s.version).size(8.5).color(Color32::from_rgb(148, 163, 184)));
                            })
                            .response
                        });

                        // Port
                        let port_label = match (s.port, s.aux_port) {
                            (0, _) => "—".to_string(),
                            (p, Some(aux)) => format!("{p}/{aux}"),
                            (p, None) => p.to_string(),
                        };
                        ui.add_sized([port_w, 18.0], egui::Label::new(RichText::new(port_label).monospace().size(10.0)));

                        // Status badge
                        ui.add_sized([status_w, 18.0], |ui: &mut egui::Ui| {
                            let starting = (s.bin_section == Some("mysql") && self.state.mysql_status == crate::mysql::DbStatus::Starting)
                                || (s.bin_section == Some("redis") && self.state.redis_status == crate::mysql::DbStatus::Starting);
                            if starting {
                                ui.label(RichText::new("STARTING").color(Color32::from_rgb(251, 191, 36)).strong().size(10.0))
                            } else if s.running {
                                ui.label(RichText::new("RUNNING").color(Color32::from_rgb(16, 185, 129)).strong().size(10.0))
                            } else if matches!(s.bin_section, Some("php") | Some("nodejs")) && !missing {
                                ui.label(RichText::new("ON DEMAND").color(Color32::from_rgb(56, 189, 248)).size(10.0))
                            } else {
                                ui.label(RichText::new("STOPPED").color(Color32::from_rgb(148, 163, 184)).size(10.0))
                            }
                        });

                        // Action buttons
                        ui.add_sized([actions_w, 18.0], |ui: &mut egui::Ui| {
                            ui.horizontal(|ui| {
                                if missing {
                                    if ui.small_button(RichText::new("⬇ Install…").color(Color32::from_rgb(56, 189, 248))).clicked() {
                                        options_idx = Some(idx);
                                    }
                                    return;
                                }
                                if MyCraftState::can_run(s) {
                                    let key_start = format!("svc_starting_{idx}");
                                    let key_stop = format!("svc_stopping_{idx}");
                                    let svc_starting = self.state.is_loading(&key_start);
                                    let svc_stopping = self.state.is_loading(&key_stop);
                                    let is_svc_loading = svc_starting || svc_stopping;

                                    let toggle_text = if svc_starting {
                                        "⏳ Starting…"
                                    } else if svc_stopping {
                                        "⏳ Stopping…"
                                    } else if s.running {
                                        "Stop"
                                    } else {
                                        "Start"
                                    };
                                    if ui.add_enabled(!is_svc_loading, egui::Button::new(toggle_text).small()).clicked() {
                                        toggle_idx = Some(idx);
                                    }

                                    let key_reload = format!("svc_reload_{idx}");
                                    let loading_reload = self.state.is_loading(&key_reload);
                                    let reload_text = if loading_reload { "⏳ Restarting…" } else { "Restart" };
                                    if ui.add_enabled(s.running && !loading_reload, egui::Button::new(reload_text).small()).clicked() {
                                        reload_idx = Some(idx);
                                    }
                                }
                                if MyCraftState::config_file(s).is_some() && ui.small_button("Config").clicked() {
                                    config_idx = Some(idx);
                                }
                                if ui.small_button("⚙ Options").clicked() {
                                    options_idx = Some(idx);
                                }
                            })
                            .response
                        });
                    });

                    if idx < self.state.services.len() - 1 {
                        ui.add_space(1.0);
                    }
                }

                if let Some(idx) = toggle_idx {
                    if let Some(s) = self.state.services.get(idx) {
                        if s.running {
                            self.state.set_loading(&format!("svc_stopping_{idx}"), 700);
                        } else {
                            self.state.set_loading(&format!("svc_starting_{idx}"), 700);
                        }
                    }
                    self.state.toggle_service(idx);
                }
                if let Some(idx) = reload_idx {
                    self.state.set_loading(&format!("svc_reload_{idx}"), 800);
                    self.state.restart_service(idx);
                }
                if let Some(idx) = config_idx {
                    self.state.open_config(idx);
                }
                if let Some(idx) = options_idx {
                    self.state.service_options_idx = Some(idx);
                }
            });

        ui.add_space(4.0);

        // Fluid grid overview cards: PHP Version, Virtual Hosts & DNS, Mail Catcher
        let mut open_vhosts = false;
        let mut open_mailpit = false;
        let mut open_php_options = false;

        let current_php_ver = crate::php_update::installed(&crate::mycraft_root().join("bin").join("php"))
            .into_iter()
            .max_by_key(|b| (b.version.clone(), b.rebuild))
            .map(|b| format!("newest {}", b.display()))
            .unwrap_or_else(|| "none installed".into());

        ui.columns(3, |cols| {
            // Card 1: PHP Version & Quick Switch
            let resp = egui::Frame::none()
                .fill(Color32::from_rgb(24, 33, 47))
                .stroke(Stroke::new(1.0_f32, Color32::from_rgb(45, 55, 72)))
                .rounding(6.0)
                .inner_margin(egui::Margin::symmetric(10.0, 5.0))
                .show(&mut cols[0], |ui| {
                    ui.set_width(ui.available_width());
                    ui.set_min_height(32.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("PHP (per project)").size(9.0).color(Color32::from_rgb(148, 163, 184)));
                        ui.add_space(1.0);
                        ui.label(RichText::new(current_php_ver).strong().size(10.0));
                    });
                })
                .response;

            if resp.interact(egui::Sense::click()).clicked() {
                open_php_options = true;
            }

            // Card 2: Virtual Hosts & DNS
            let suffix = crate::settings::extract_suffix(&self.state.domain);
            let (vhost_label, vhost_color) = if self.state.dns_wired {
                (format!("⚫ All *{suffix} in hosts"), Color32::from_rgb(16, 185, 129))
            } else {
                (format!("⚫ *{suffix} missing in hosts"), Color32::from_rgb(239, 68, 68))
            };

            let resp = egui::Frame::none()
                .fill(Color32::from_rgb(24, 33, 47))
                .stroke(Stroke::new(1.0_f32, Color32::from_rgb(45, 55, 72)))
                .rounding(6.0)
                .inner_margin(egui::Margin::symmetric(10.0, 5.0))
                .show(&mut cols[1], |ui| {
                    ui.set_width(ui.available_width());
                    ui.set_min_height(32.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Virtual Hosts & DNS").size(9.0).color(Color32::from_rgb(148, 163, 184)));
                        ui.add_space(1.0);
                        ui.label(RichText::new(vhost_label).strong().size(10.0).color(vhost_color));
                    });
                })
                .response;

            if resp.interact(egui::Sense::click()).clicked() {
                open_vhosts = true;
            }

            // Card 3: Mail Catcher
            let mailpit_running = self.state.services.iter()
                .find(|s| s.name.contains("Mailpit"))
                .map(|s| s.running)
                .unwrap_or(false);

            let (mail_label, mail_color) = if mailpit_running {
                (format!("Mailpit (web :{})", self.state.mailpit_port), Color32::from_rgb(16, 185, 129))
            } else {
                (format!("Mailpit (click to start :{})", self.state.mailpit_port), Color32::from_rgb(251, 191, 36))
            };

            let resp = egui::Frame::none()
                .fill(Color32::from_rgb(24, 33, 47))
                .stroke(Stroke::new(1.0_f32, Color32::from_rgb(45, 55, 72)))
                .rounding(6.0)
                .inner_margin(egui::Margin::symmetric(10.0, 5.0))
                .show(&mut cols[2], |ui| {
                    ui.set_width(ui.available_width());
                    ui.set_min_height(32.0);
                    ui.vertical(|ui| {
                        ui.label(RichText::new("Mail Catcher (Click to Open)").size(9.0).color(Color32::from_rgb(148, 163, 184)));
                        ui.add_space(1.0);
                        ui.label(RichText::new(mail_label).strong().size(10.0).color(mail_color));
                    });
                })
                .response;

            if resp.interact(egui::Sense::click()).clicked() {
                open_mailpit = true;
            }
        });

        if open_vhosts {
            self.state.active_tab = ActiveTab::VirtualHosts;
        }
        if open_mailpit {
            if let Some(idx) = self.state.services.iter().position(|s| s.name.contains("Mailpit") && !s.running) {
                self.state.toggle_service(idx);
            }
            let url = format!("http://localhost:{}", self.state.mailpit_port);
            self.open_url(&url);
        }
        if open_php_options {
            self.state.service_options_idx = Some(2);
        }
    }

    fn render_quick_app(&mut self, ui: &mut egui::Ui) {
        let suffix = crate::settings::extract_suffix(&self.state.domain);
        ui.heading("Quick App Scaffolder");
        ui.label(RichText::new(format!("Creates the project in www\\<name>, its database (WordPress/Laravel, when MySQL runs) and its <name>{suffix} hosts entry."))
            .color(Color32::from_rgb(148, 163, 184)));

        ui.add_space(8.0);

        ui.horizontal(|ui| {
            ui.label(RichText::new("Project Folder:").strong());
            ui.add(
                egui::TextEdit::singleline(&mut self.state.new_app_name)
                    .hint_text("e.g. mycraft")
                    .desired_width(180.0),
            );

            let proj_name = if self.state.new_app_name.trim().is_empty() {
                "mycraft"
            } else {
                self.state.new_app_name.trim()
            };
            let current_domain = crate::settings::format_hostname(&self.state.domain, proj_name);
            ui.label(
                RichText::new(format!("→ http://{}", current_domain))
                    .color(Color32::from_rgb(56, 189, 248))
                    .size(11.0),
            );
        });

        ui.add_space(8.0);
        ui.label(RichText::new("Select Framework Template:").strong());

        let templates = MyCraftState::templates();
        let mut selected_idx = None;
        let current_selected = self.state.selected_template;

        ui.columns(2, |cols| {
            // Left column: templates 0, 2, 4
            for &idx in &[0usize, 2, 4] {
                if let Some(t) = templates.get(idx) {
                    let is_selected = current_selected == idx;
                    let (rect, response) = cols[0].allocate_exact_size(
                        Vec2::new(cols[0].available_width(), 48.0),
                        egui::Sense::click(),
                    );

                    if response.clicked() {
                        selected_idx = Some(idx);
                    }

                    let bg_color = if is_selected {
                        Color32::from_rgb(30, 58, 138)
                    } else if response.hovered() {
                        Color32::from_rgb(36, 48, 68)
                    } else {
                        Color32::from_rgb(24, 33, 47)
                    };

                    let border_color = if is_selected {
                        Color32::from_rgb(56, 189, 248)
                    } else {
                        Color32::from_rgb(45, 55, 72)
                    };

                    cols[0].painter().rect(
                        rect,
                        6.0,
                        bg_color,
                        Stroke::new(if is_selected { 1.5_f32 } else { 1.0_f32 }, border_color),
                    );

                    cols[0].painter().text(
                        rect.left_top() + Vec2::new(8.0, 6.0),
                        egui::Align2::LEFT_TOP,
                        t.name,
                        egui::FontId::proportional(12.0),
                        Color32::WHITE,
                    );

                    cols[0].painter().text(
                        rect.right_top() + Vec2::new(-8.0, 7.0),
                        egui::Align2::RIGHT_TOP,
                        t.badge,
                        egui::FontId::proportional(9.5),
                        Color32::from_rgb(56, 189, 248),
                    );

                    cols[0].painter().text(
                        rect.left_top() + Vec2::new(8.0, 27.0),
                        egui::Align2::LEFT_TOP,
                        t.description,
                        egui::FontId::proportional(10.5),
                        Color32::from_rgb(148, 163, 184),
                    );

                    cols[0].add_space(6.0);
                }
            }

            // Right column: templates 1, 3, 5
            for &idx in &[1usize, 3, 5] {
                if let Some(t) = templates.get(idx) {
                    let is_selected = current_selected == idx;
                    let (rect, response) = cols[1].allocate_exact_size(
                        Vec2::new(cols[1].available_width(), 48.0),
                        egui::Sense::click(),
                    );

                    if response.clicked() {
                        selected_idx = Some(idx);
                    }

                    let bg_color = if is_selected {
                        Color32::from_rgb(30, 58, 138)
                    } else if response.hovered() {
                        Color32::from_rgb(36, 48, 68)
                    } else {
                        Color32::from_rgb(24, 33, 47)
                    };

                    let border_color = if is_selected {
                        Color32::from_rgb(56, 189, 248)
                    } else {
                        Color32::from_rgb(45, 55, 72)
                    };

                    cols[1].painter().rect(
                        rect,
                        6.0,
                        bg_color,
                        Stroke::new(if is_selected { 1.5_f32 } else { 1.0_f32 }, border_color),
                    );

                    cols[1].painter().text(
                        rect.left_top() + Vec2::new(8.0, 6.0),
                        egui::Align2::LEFT_TOP,
                        t.name,
                        egui::FontId::proportional(12.0),
                        Color32::WHITE,
                    );

                    cols[1].painter().text(
                        rect.right_top() + Vec2::new(-8.0, 7.0),
                        egui::Align2::RIGHT_TOP,
                        t.badge,
                        egui::FontId::proportional(9.5),
                        Color32::from_rgb(56, 189, 248),
                    );

                    cols[1].painter().text(
                        rect.left_top() + Vec2::new(8.0, 27.0),
                        egui::Align2::LEFT_TOP,
                        t.description,
                        egui::FontId::proportional(10.5),
                        Color32::from_rgb(148, 163, 184),
                    );

                    cols[1].add_space(6.0);
                }
            }
        });

        if let Some(idx) = selected_idx {
            self.state.selected_template = idx;
        }

        ui.add_space(6.0);

        ui.horizontal(|ui| {
            let is_loading = self.state.quick_busy || self.state.is_loading("create_project");
            let btn_text = if is_loading { "⏳ Creating Project…" } else { "⚡ Create Project" };
            let create_btn = egui::Button::new(
                RichText::new(btn_text)
                    .strong()
                    .color(Color32::from_rgb(15, 23, 42)),
            )
            .fill(Color32::from_rgb(16, 185, 129))
            .min_size(Vec2::new(140.0, 30.0));

            if ui.add_enabled(!is_loading, create_btn).clicked() {
                self.state.set_loading("create_project", 1000);
                self.state.create_project();
            }
            if is_loading {
                ui.spinner();
            }

            if self.state.quick_busy {
                ui.label(
                    RichText::new("Creating project… (see progress on Log menu)")
                        .color(Color32::from_rgb(56, 189, 248))
                        .size(11.0),
                );
                if ui.button(RichText::new("📜 See Log").size(11.0)).clicked() {
                    self.state.active_tab = ActiveTab::Logs;
                }
            } else if let Some(status) = &self.state.quick_create_status {
                ui.label(RichText::new(status).color(Color32::from_rgb(56, 189, 248)).size(11.0));
                if let Some(start) = status.find("http://") {
                    let url_candidate = &status[start..];
                    let url = url_candidate
                        .split_whitespace()
                        .next()
                        .unwrap_or(url_candidate)
                        .trim_end_matches([')', ']', '.', ','])
                        .to_string();
                    if ui.button(RichText::new("🌐 Open Site").size(11.0).color(Color32::from_rgb(16, 185, 129))).clicked() {
                        self.open_url(&url);
                    }
                }
            }
        });
    }

    fn render_virtual_hosts(&mut self, ui: &mut egui::Ui) {
        let suffix = crate::settings::extract_suffix(&self.state.domain);
        ui.heading("Virtual Hosts & Local DNS");
        ui.label(RichText::new(format!("Local *{suffix} domains automatically routed with DNS mapping to your www folders."))
            .color(Color32::from_rgb(148, 163, 184)));

        ui.add_space(6.0);

        // DNS & Privilege Diagnostic Status Card
        let (card_bg, card_border) = if !self.state.is_admin || !self.state.dns_wired {
            (Color32::from_rgb(45, 26, 15), Color32::from_rgb(217, 119, 6))
        } else {
            (Color32::from_rgb(15, 35, 24), Color32::from_rgb(16, 185, 129))
        };

        egui::Frame::none()
            .fill(card_bg)
            .stroke(Stroke::new(1.0_f32, card_border))
            .rounding(6.0)
            .inner_margin(egui::Margin::symmetric(10.0, 7.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Privilege & DNS Status:").strong().size(11.0));

                    if self.state.is_admin {
                        ui.label(RichText::new("⚫ Admin (Elevated)").size(10.5).color(Color32::from_rgb(56, 189, 248)));
                    } else {
                        ui.label(RichText::new("⚫ Non-Admin (Hosts read-only)").size(10.5).color(Color32::from_rgb(251, 191, 36)));
                    }

                    ui.label("•");

                    if self.state.dns_wired {
                        ui.label(RichText::new(format!("⚫ DNS Wired (*{suffix} active)")).size(10.5).color(Color32::from_rgb(16, 185, 129)));
                    } else {
                        ui.label(RichText::new("⚫ DNS Not Wired (ERR_NAME_NOT_RESOLVED)").size(10.5).color(Color32::from_rgb(239, 68, 68)));
                    }

                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let loading_recheck = self.state.is_loading("vhost_recheck");
                        let recheck_text = if loading_recheck { "⏳ Checking…" } else { "🔄 Recheck DNS" };
                        if ui.add_enabled(!loading_recheck, egui::Button::new(RichText::new(recheck_text).size(10.0)).small()).clicked() {
                            self.state.set_loading("vhost_recheck", 600);
                            self.state.recheck_admin_and_dns();
                        }

                        let loading_wire = self.state.is_loading("vhost_wire");
                        let wire_text = if loading_wire { "⏳ Wiring…" } else { "⚡ Wire DNS" };
                        if ui.add_enabled(!loading_wire, egui::Button::new(RichText::new(wire_text).size(10.0)).small()).clicked() {
                            self.state.set_loading("vhost_wire", 1000);
                            self.state.wire_dns();
                        }

                        if !self.state.is_admin {
                            let loading_admin = self.state.is_loading("vhost_restart_admin");
                            let admin_text = if loading_admin { "⏳ Restarting…" } else { "🛡 Restart Admin" };
                            if ui.add_enabled(!loading_admin, egui::Button::new(RichText::new(admin_text).size(10.0)).small()).clicked() {
                                self.state.set_loading("vhost_restart_admin", 1000);
                                self.state.restart_as_admin();
                            }
                        }
                    });
                });

                if !self.state.dns_wired {
                    ui.add_space(2.0);
                    ui.label(RichText::new(format!("💡 Without a hosts entry, open a project at http://localhost:{}/<folder>", self.state.web_port))
                        .size(10.0).color(Color32::from_rgb(254, 243, 199)));
                }
            });

        ui.add_space(6.0);

        // Explanatory banner for project stack connector
        egui::Frame::none()
            .fill(Color32::from_rgb(15, 23, 42))
            .stroke(Stroke::new(1.0_f32, Color32::from_rgb(56, 189, 248)))
            .rounding(6.0)
            .inner_margin(egui::Margin::symmetric(10.0, 6.0))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("⚡ PHP per project:").strong().color(Color32::from_rgb(56, 189, 248)).size(11.0));
                    ui.label(RichText::new("Auto follows composer.json; pick a version to pin it (.php-version). Changes apply on the next request.")
                        .size(10.5).color(Color32::from_rgb(203, 213, 225)));
                });
            });

        ui.add_space(6.0);

        // Search filter input bar
        ui.horizontal(|ui| {
            ui.label(RichText::new("🔍 Search Projects:").strong().size(11.0));
            ui.add(
                egui::TextEdit::singleline(&mut self.state.vhost_search)
                    .hint_text("Filter by name...")
                    .desired_width(180.0),
            );
            if !self.state.vhost_search.is_empty() && ui.small_button("✕ Clear").clicked() {
                self.state.vhost_search.clear();
            }
        });

        ui.add_space(4.0);

        egui::ScrollArea::vertical().show(ui, |ui| {
            let root = PathBuf::from(&self.state.www_root);
            let mut dirs = Vec::new();
            if let Ok(entries) = std::fs::read_dir(&root) {
                for e in entries.flatten() {
                    if let Ok(ft) = e.file_type() {
                        if ft.is_dir() {
                            let name = e.file_name().to_string_lossy().to_string();
                            // Exclude any folders that start with dot (e.g. .claude, .pnpm-store) or underscore
                            if name.starts_with('.') || name.starts_with('_') {
                                continue;
                            }
                            dirs.push(name);
                        }
                    }
                }
            }

            dirs.sort();

            if !self.state.vhost_search.trim().is_empty() {
                let q = self.state.vhost_search.trim().to_lowercase();
                dirs.retain(|d| d.to_lowercase().contains(&q));
            }

            let mut set_php: Option<(String, Option<String>)> = None;
            let php_bin = crate::mycraft_root().join("bin").join("php");
            let mut php_lines: Vec<String> = crate::php_update::installed(&php_bin).iter().map(|b| b.line()).collect();
            php_lines.sort_by_key(|l| std::cmp::Reverse(l.split('.').map(|p| p.parse::<u32>().unwrap_or(0)).collect::<Vec<_>>()));
            php_lines.dedup();
            let mut start_tunnel_proj: Option<String> = None;
            let mut stop_tunnel_proj: Option<String> = None;

            if dirs.is_empty() {
                ui.label("No folders found in www root.");
            } else {
                for dir in dirs {
                    let dir_clone = dir.clone();
                    let tunnel_status = self.state.get_tunnel_status(&dir);

                    egui::Frame::none()
                        .fill(Color32::from_rgb(24, 33, 47))
                        .rounding(4.0)
                        .inner_margin(egui::Margin::symmetric(8.0, 6.0))
                        .show(ui, |ui| {
                            let host_name = crate::settings::format_hostname(&self.state.domain, &dir);
                            // Rows wrap so the card fits the default 540px window.
                            // Row 1: URL + DNS state
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(format!("http://{host_name}")).strong().color(Color32::from_rgb(56, 189, 248)));
                                if self.state.dns_wired {
                                    ui.label(RichText::new(format!("(127.0.0.1 • :{})", self.state.web_port)).size(9.5).color(Color32::from_rgb(148, 163, 184)));
                                } else {
                                    ui.label(RichText::new("[⚠ DNS Unwired]").size(9.0).color(Color32::from_rgb(239, 68, 68)));
                                }
                            });

                            // Row 2: stack + open actions
                            ui.add_space(3.0);
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new("PHP:").size(10.0).color(Color32::from_rgb(148, 163, 184)));

                                // The PHP this project actually runs on: `.php-version`
                                // pin, else composer.json's require.php, else newest.
                                let proj_dir = root.join(&dir);
                                let pin = crate::php_update::pinned(&proj_dir);
                                let resolved = crate::php_update::for_project(&php_bin, &proj_dir);
                                let (label, color) = match (&pin, &resolved) {
                                    (_, Err(_)) => (pin.clone().map_or("missing".into(), |p| format!("{p} (missing)")), Color32::from_rgb(239, 68, 68)),
                                    (Some(_), Ok(b)) => (b.display(), Color32::from_rgb(56, 189, 248)),
                                    (None, Ok(b)) => (format!("Auto · {}", b.display()), Color32::from_rgb(56, 189, 248)),
                                };
                                let mut hover = match &resolved {
                                    Err(e) => e.clone(),
                                    Ok(_) if pin.is_some() => "Pinned in .php-version".to_string(),
                                    Ok(_) => crate::php_update::composer_php(&proj_dir)
                                        .map_or("Newest installed PHP".to_string(), |c| format!("Newest installed PHP matching composer.json ({c})")),
                                };
                                hover.push_str("\nRuns on the newest patch of the chosen line.");

                                egui::ComboBox::from_id_salt(format!("php_sel_{}", dir))
                                    .selected_text(RichText::new(label).size(10.5).color(color))
                                    .width(110.0)
                                    .show_ui(ui, |ui| {
                                        if ui.selectable_label(pin.is_none(), "Auto (composer.json)").clicked() {
                                            set_php = Some((dir_clone.clone(), None));
                                        }
                                        for line in &php_lines {
                                            if ui.selectable_label(pin.as_deref() == Some(line.as_str()), format!("PHP {line}")).clicked() {
                                                set_php = Some((dir_clone.clone(), Some(line.clone())));
                                            }
                                        }
                                    })
                                    .response
                                    .on_hover_text(hover);

                                let key_open = format!("vhost_open_{dir}");
                                let loading_open = self.state.is_loading(&key_open);
                                let open_label = if loading_open { "⏳ Opening…" } else { "Open Browser" };
                                if ui.add_enabled(!loading_open, egui::Button::new(RichText::new(open_label).size(10.5)).small()).clicked() {
                                    self.state.set_loading(&key_open, 600);
                                    let url = if self.state.dns_wired {
                                        format!("http://{host_name}")
                                    } else {
                                        format!("http://localhost:{}/{}", self.state.web_port, dir)
                                    };
                                    self.open_url(&url);
                                }

                                if !self.state.dns_wired {
                                    let key_open_go = format!("vhost_opengo_{dir}");
                                    let loading_go = self.state.is_loading(&key_open_go);
                                    let go_label = if loading_go { "⏳ Opening…".to_string() } else { format!("Open {suffix}") };
                                    if ui.add_enabled(!loading_go, egui::Button::new(RichText::new(go_label).size(10.5)).small()).clicked() {
                                        self.state.set_loading(&key_open_go, 600);
                                        let url = format!("http://{host_name}");
                                        self.open_url(&url);
                                    }
                                }
                            });

                            // Row 3: Pinggy Public Tunnel controls & status
                            ui.add_space(3.0);
                            ui.horizontal_wrapped(|ui| {
                                match tunnel_status {
                                    None => {
                                        let key_tun = format!("tun_start_{dir}");
                                        let loading_tun = self.state.is_loading(&key_tun);
                                        let tun_label = if loading_tun { "⏳ Starting…" } else { "🌐 Make Online" };
                                        if ui.add_enabled(!loading_tun, egui::Button::new(RichText::new(tun_label).size(10.5).strong().color(Color32::from_rgb(56, 189, 248)))).clicked() {
                                            self.state.set_loading(&key_tun, 800);
                                            start_tunnel_proj = Some(dir_clone.clone());
                                        }
                                        ui.label(RichText::new("Public Pinggy tunnel (zero-install SSH)").size(9.5).color(Color32::from_rgb(100, 116, 139)));
                                    }
                                    Some(crate::tunnel::TunnelStatus::Starting) => {
                                        ui.label(RichText::new("⏳ Connecting Pinggy SSH tunnel...").size(10.5).color(Color32::from_rgb(251, 191, 36)));
                                        if ui.small_button("Cancel").clicked() {
                                            stop_tunnel_proj = Some(dir_clone.clone());
                                        }
                                    }
                                    Some(crate::tunnel::TunnelStatus::Online { url, local_port }) => {
                                        ui.label(RichText::new("🟢 Online:").size(10.5).strong().color(Color32::from_rgb(16, 185, 129)));
                                        ui.label(RichText::new(&url).size(10.5).strong().color(Color32::from_rgb(56, 189, 248)));
                                        ui.label(RichText::new(format!("(:{})", local_port)).size(9.0).color(Color32::from_rgb(148, 163, 184)));

                                        if ui.small_button("📋 Copy").clicked() {
                                            ui.ctx().copy_text(url.clone());
                                            self.state.log(crate::state::LogLevel::Success, &format!("Copied Pinggy URL: {}", url));
                                        }
                                        if ui.small_button("↗ Open").clicked() {
                                            self.open_url(&url);
                                        }
                                        if ui.small_button(RichText::new("⏹ Stop").color(Color32::from_rgb(239, 68, 68))).clicked() {
                                            stop_tunnel_proj = Some(dir_clone.clone());
                                        }
                                    }
                                    Some(crate::tunnel::TunnelStatus::Error(err)) => {
                                        ui.label(RichText::new(format!("⚠ Tunnel Error: {}", err)).size(10.0).color(Color32::from_rgb(239, 68, 68)));
                                        if ui.small_button("🔄 Retry").clicked() {
                                            start_tunnel_proj = Some(dir_clone.clone());
                                        }
                                        if ui.small_button("✕ Dismiss").clicked() {
                                            stop_tunnel_proj = Some(dir_clone.clone());
                                        }
                                    }
                                }
                            });
                        });
                    ui.add_space(4.0);
                }
            }

            if let Some(proj) = start_tunnel_proj {
                self.state.start_tunnel(&proj);
            }
            if let Some(proj) = stop_tunnel_proj {
                self.state.stop_tunnel(&proj);
            }
            if let Some((proj, line)) = set_php {
                self.state.set_project_php(&proj, line.as_deref());
            }
        });
    }

    fn render_service_options_modal(&mut self, ctx: &egui::Context, s_idx: usize) {
        if s_idx >= self.state.services.len() {
            self.state.service_options_idx = None;
            return;
        }

        let s = &self.state.services[s_idx];
        let s_name = s.name.clone();
        let s_ver = s.version.clone();
        let s_port = s.port;
        let s_running = s.running;
        let avail_vers = s.available_versions.clone();
        let s_section = s.bin_section;

        let mut close_modal = false;
        let mut switch_to: Option<String> = None;
        let mut open_db = false;
        let mut open_config_target: Option<usize> = None;
        let mut open_url_target: Option<String> = None;
        let mut log_message: Option<String> = None;
        let mut toggle = false;

        egui::Window::new(format!("⚙ Service Options: {}", s_name))
            .collapsible(false)
            .resizable(true)
            .default_size([480.0, 380.0])
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&s_name).strong().size(13.0));
                        let status_badge = if s_running {
                            RichText::new("⚫ RUNNING").color(Color32::from_rgb(16, 185, 129)).strong().size(10.5)
                        } else {
                            RichText::new("⚫ STOPPED").color(Color32::from_rgb(148, 163, 184)).size(10.5)
                        };
                        ui.label(status_badge);
                        if s_port != 0 {
                            ui.label(RichText::new(format!("• Port {}", s_port)).size(10.5).color(Color32::from_rgb(148, 163, 184)));
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("✕ Close").clicked() {
                                close_modal = true;
                            }
                        });
                    });

                    ui.separator();

                    if let Some(section) = s_section {
                        self.render_package_installs(ui, section);
                        if section == "mysql" {
                            self.render_package_installs(ui, "heidisql");
                        }
                        ui.separator();
                    }

                    // PHP: the real builds in bin\php. There's no global switch;
                    // each project picks its PHP (card dropdown / composer.json).
                    if s_name.contains("PHP") {
                        let php_bin = crate::mycraft_root().join("bin").join("php");
                        let mut builds = crate::php_update::installed(&php_bin);
                        builds.sort_by_key(|b| std::cmp::Reverse((b.version.clone(), b.rebuild)));
                        ui.label(RichText::new("📦 Installed PHP Builds").strong().size(11.5).color(Color32::from_rgb(56, 189, 248)));
                        ui.label(RichText::new(format!("In {}. Each project runs on the newest patch of the PHP it needs (set it on the project card).", php_bin.display()))
                            .size(10.0).color(Color32::from_rgb(148, 163, 184)));
                        ui.add_space(4.0);
                        if builds.is_empty() {
                            ui.label(RichText::new("No PHP installed yet.").size(11.0).color(Color32::from_rgb(239, 68, 68)));
                        }
                        for b in &builds {
                            let rebuild = if b.rebuild > 0 { format!("-{}", b.rebuild) } else { String::new() };
                            ui.label(RichText::new(format!("PHP {}{}  ({}, {})", b.display(), rebuild, b.vc, b.arch)).size(11.0));
                        }
                    } else if s_section.is_none() {
                        ui.label(RichText::new("Built into myCraft: nothing to install or switch.").size(10.5).color(Color32::from_rgb(148, 163, 184)));
                    } else {
                        ui.label(RichText::new("⚡ Version").strong().size(11.5).color(Color32::from_rgb(56, 189, 248)));
                        ui.label(RichText::new("Installed in bin; switching restarts the service if it runs.").size(10.0).color(Color32::from_rgb(148, 163, 184)));
                        ui.add_space(4.0);
                        for v in &avail_vers {
                            ui.horizontal(|ui| {
                                if v == &s_ver {
                                    ui.label(RichText::new(format!("✓ {v} (active)")).strong().size(11.0).color(Color32::from_rgb(16, 185, 129)));
                                } else if ui.button(RichText::new(format!("Switch to {v}")).size(11.0)).clicked() {
                                    switch_to = Some(v.clone());
                                }
                            });
                        }
                    }
                    if let Some(section) = s_section {
                        if ui.small_button(format!("📂 Open bin\\{section}")).clicked() {
                            let dir = crate::packages::bin_dir(section);
                            let _ = std::fs::create_dir_all(&dir);
                            let _ = Command::new("explorer.exe").arg(&dir).spawn();
                        }
                    }

                    ui.add_space(8.0);
                    ui.separator();
                    ui.label(RichText::new("🛠 Tools").strong().size(11.5).color(Color32::from_rgb(56, 189, 248)));
                    ui.add_space(4.0);

                    let logs = crate::mycraft_root().join("logs");
                    if s_section.is_none() && !s_name.contains("Mailpit") {
                        let ca = crate::ssl::ca_cert_path();
                        let suffix = crate::settings::extract_suffix(&self.state.domain);
                        let ssl_text = match (ca.is_file(), self.state.ssl_trusted) {
                            (_, true) => format!("HTTPS: myCraft CA trusted; https://<project>{suffix} on port {}.", self.state.https_port),
                            (true, false) => "HTTPS: certificate made but not trusted yet, so browsers warn.".to_string(),
                            (false, false) => "HTTPS: no certificate yet (made on the first https request or when you trust it).".to_string(),
                        };
                        ui.label(RichText::new(ssl_text).size(10.5));
                        ui.horizontal_wrapped(|ui| {
                            let loading_trust = self.state.is_loading("trust_ssl");
                            let trust_text = if loading_trust { "⏳ Trusting…" } else { "🔒 Trust HTTPS certificate" };
                            if ui.add_enabled(!self.state.ssl_trusted && !loading_trust, egui::Button::new(trust_text)).clicked() {
                                self.state.set_loading("trust_ssl", 2000);
                                self.state.trust_ssl();
                            }
                            if ui.button("📁 etc\\ssl").clicked() {
                                let _ = crate::ssl::ensure_ca();
                                let _ = Command::new("explorer.exe").arg(crate::ssl::dir()).spawn();
                            }
                            if ui.button("📜 access.log").clicked() {
                                self.open_log(logs.join("access.log"));
                            }
                            if ui.button("📜 php_error.log").clicked() {
                                self.open_log(logs.join("php_error.log"));
                            }
                        });
                    } else if s_name.contains("MySQL") || s_name.contains("MariaDB") {
                        let status = match &self.state.mysql_status {
                            crate::mysql::DbStatus::Running => format!("Running on 127.0.0.1:{} (user root).", self.state.mysql_port),
                            crate::mysql::DbStatus::Starting => "Starting…".to_string(),
                            crate::mysql::DbStatus::Stopped => "Stopped.".to_string(),
                            crate::mysql::DbStatus::Failed(e) => format!("Failed: {e}"),
                        };
                        ui.label(RichText::new(status).size(10.5));
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("🐬 Open HeidiSQL").clicked() {
                                open_db = true;
                            }
                            if ui.button("🔑 Set Random Root Password").on_hover_text("Needs the server running and root without a password; the new one is copied to the clipboard.").clicked() {
                                self.state.reset_mysql_root_password();
                            }
                            if ui.button("📄 my.ini").clicked() {
                                open_config_target = Some(s_idx);
                            }
                            let data_dir = crate::mysql::Install::new(crate::mycraft_root(), &s_ver).map(|i| i.data_dir);
                            if ui.add_enabled(data_dir.is_ok(), egui::Button::new("📁 Data Dir")).clicked() {
                                if let Ok(d) = &data_dir {
                                    let _ = Command::new("explorer.exe").arg(if d.exists() { d.clone() } else { crate::mycraft_root().join("data") }).spawn();
                                }
                            }
                            if ui.button("📜 Server Log").clicked() {
                                self.open_log(crate::mysql::log_path());
                            }
                        });
                    } else if s_name.contains("PHP") {
                        let php_bin = crate::mycraft_root().join("bin").join("php");
                        let mut builds = crate::php_update::installed(&php_bin);
                        builds.sort_by_key(|b| std::cmp::Reverse((b.version.clone(), b.rebuild)));
                        for b in &builds {
                            let dir = php_bin.join(&b.dir_name);
                            ui.horizontal_wrapped(|ui| {
                                ui.label(RichText::new(format!("PHP {}", b.display())).size(10.5));
                                if ui.small_button("📄 php.ini").on_hover_text("This build's php.ini (made from php.ini-development if missing).").clicked() {
                                    match crate::php_update::ensure_ini(&dir) {
                                        Ok((ini, created)) => {
                                            if created {
                                                log_message = Some(format!("Created {} with the common extensions on.", ini.display()));
                                            }
                                            let _ = Command::new("notepad.exe").arg(ini).spawn();
                                        }
                                        Err(e) => self.state.log(LogLevel::Error, &e),
                                    }
                                }
                                if ui.small_button("ℹ phpinfo()").clicked() {
                                    match crate::php_update::phpinfo_html(&dir) {
                                        Ok(html) => open_url_target = Some(html.to_string_lossy().to_string()),
                                        Err(e) => self.state.log(LogLevel::Error, &format!("phpinfo() for PHP {}: {e}", b.display())),
                                    }
                                }
                                if ui.small_button("📁 ext").clicked() {
                                    let _ = Command::new("explorer.exe").arg(dir.join("ext")).spawn();
                                }
                            });
                        }
                        self.render_php_updates(ui);
                        ui.add_space(4.0);
                        self.render_package_installs(ui, "composer");
                    } else if s_name.contains("Node") {
                        let node = crate::quickapp::newest_node();
                        match &node {
                            Ok(dir) => ui.label(RichText::new(format!("Terminals get {} first on PATH; global npm packages go in its node_modules.", dir.display())).size(10.5)),
                            Err(e) => ui.label(RichText::new(e).size(10.5)),
                        };
                        if let Ok(dir) = node {
                            ui.horizontal_wrapped(|ui| {
                                if ui.button("💻 Node.js Terminal").clicked() {
                                    self.open_console("Node.js Terminal", &dir, None);
                                }
                                if ui.button("📦 Global packages").clicked() {
                                    let _ = Command::new("explorer.exe").arg(dir.join("node_modules")).spawn();
                                }
                            });
                        }
                    } else if s_name.contains("Redis") {
                        let status = match &self.state.redis_status {
                            crate::mysql::DbStatus::Running => format!("Running on 127.0.0.1:{}.", self.state.redis_port),
                            crate::mysql::DbStatus::Starting => "Starting…".to_string(),
                            crate::mysql::DbStatus::Stopped => "Stopped.".to_string(),
                            crate::mysql::DbStatus::Failed(e) => format!("Failed: {e}"),
                        };
                        ui.label(RichText::new(status).size(10.5));
                        let target = self.state.redis.target();
                        ui.horizontal_wrapped(|ui| {
                            if ui.button("📄 redis.conf").clicked() {
                                open_config_target = Some(s_idx);
                            }
                            if ui.add_enabled(target.is_some(), egui::Button::new("⚡ FLUSHALL")).on_hover_text("Deletes every key in every database.").clicked() {
                                if let Some((folder, port)) = &target {
                                    match crate::redis::cli(folder, *port, &["FLUSHALL"]) {
                                        Ok(out) => self.state.log(LogLevel::Success, &format!("Redis FLUSHALL: {out}")),
                                        Err(e) => self.state.log(LogLevel::Error, &format!("Redis FLUSHALL failed: {e}")),
                                    }
                                }
                            }
                            if ui.add_enabled(target.is_some(), egui::Button::new("💻 Redis CLI")).clicked() {
                                if let Some((folder, port)) = &target {
                                    if let Some(exe) = crate::redis::tool(folder, "redis-cli.exe") {
                                        let dir = exe.parent().map(PathBuf::from).unwrap_or_default();
                                        self.open_console("Redis CLI", &dir, Some((exe, vec!["-p".into(), port.to_string()])));
                                    }
                                }
                            }
                            if ui.button("📜 Server Log").clicked() {
                                self.open_log(crate::redis::log_path());
                            }
                        });
                    } else if s_name.contains("Mailpit") {
                        let (web, smtp) = (self.state.mailpit_port, self.state.smtp_port);
                        ui.label(RichText::new(format!("Apps send mail to SMTP 127.0.0.1:{smtp} (no auth); it lands in the web inbox on :{web}.")).size(10.5));
                        ui.horizontal_wrapped(|ui| {
                            let label = if s_running { "■ Stop Mailpit" } else { "▶ Start Mailpit" };
                            if ui.button(RichText::new(label).strong()).clicked() {
                                toggle = true;
                            }
                            if ui.button(format!("📬 Open inbox (:{web})")).clicked() {
                                toggle = !s_running;
                                open_url_target = Some(format!("http://localhost:{web}"));
                            }
                            if ui.add_enabled(s_running, egui::Button::new("🗑 Purge messages")).clicked() {
                                open_url_target = Some(format!("http://localhost:{web}/purge"));
                            }
                            if ui.add_enabled(s_running, egui::Button::new("⚡ Send test email")).clicked() {
                                open_url_target = Some(format!("http://localhost:{web}/test-email"));
                            }
                        });
                    }

                    ui.add_space(10.0);
                    if ui.button(RichText::new("Close Options").strong()).clicked() {
                        close_modal = true;
                    }
                });
            });

        if close_modal {
            self.state.service_options_idx = None;
        }
        if let Some(ver) = switch_to {
            self.state.switch_service_version(s_idx, &ver);
        }
        if toggle {
            self.state.toggle_service(s_idx);
        }
        if open_db {
            self.open_database();
        }
        if let Some(cfg_idx) = open_config_target {
            self.state.open_config(cfg_idx);
        }
        if let Some(url) = open_url_target {
            self.open_url(&url);
        }
        if let Some(msg) = log_message {
            self.state.log(LogLevel::Success, &msg);
        }
    }

    /// Install buttons for this service's entries in usr\packages.conf.
    fn render_package_installs(&mut self, ui: &mut egui::Ui, section: &str) {
        let packages: Vec<_> = crate::packages::load().into_iter().filter(|p| p.section == section).collect();
        let installed = crate::packages::installed(section);
        let (busy, message) = self.state.installer.status();

        ui.label(RichText::new("⬇ Install").strong().size(11.5).color(Color32::from_rgb(56, 189, 248)));
        ui.label(
            RichText::new(format!("Into {}. Packages come from {}.", crate::packages::bin_dir(section).display(), crate::packages::conf_path().display()))
                .size(10.0)
                .color(Color32::from_rgb(148, 163, 184)),
        );
        if installed.is_empty() {
            ui.label(RichText::new("Nothing installed yet.").size(10.5).color(Color32::from_rgb(251, 191, 36)));
        }
        ui.horizontal_wrapped(|ui| {
            if packages.is_empty() {
                ui.label(RichText::new(format!("No [{section}] packages in packages.conf.")).size(10.5));
            }
            for p in &packages {
                let label = match &p.source {
                    crate::packages::Source::Auto => format!("{} (latest)", p.name),
                    crate::packages::Source::Url { .. } => p.name.clone(),
                };
                if ui.add_enabled(busy.is_none(), egui::Button::new(RichText::new(label).size(10.5))).clicked() {
                    self.state.installer.install(p.clone());
                }
            }
            if ui.small_button("📄 Edit list").clicked() {
                let _ = Command::new("notepad.exe").arg(crate::packages::conf_path()).spawn();
            }
        });
        if busy.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(RichText::new(&message).size(10.0));
            });
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
        } else if !message.is_empty() {
            ui.label(RichText::new(&message).size(10.0).color(Color32::from_rgb(148, 163, 184)));
        }
    }

    /// Installed PHP lines vs their newest patch on windows.php.net.
    fn render_php_updates(&mut self, ui: &mut egui::Ui) {
        use crate::php_update::Status;
        let bin_dir = crate::mycraft_root().join("bin").join("php");
        let (busy, message, reports) = self.state.php_updater.snapshot();

        ui.add_space(6.0);
        ui.label(RichText::new("🛡 Security Updates").strong().size(11.5).color(Color32::from_rgb(251, 191, 36)));
        for r in &reports {
            let (text, color) = match &r.status {
                Status::Current => ("latest patch".to_string(), Color32::from_rgb(16, 185, 129)),
                Status::Outdated(rel) => (format!("update to {}", rel.version), Color32::from_rgb(251, 191, 36)),
                Status::Unsupported => ("end of life, no fixes: remove it".to_string(), Color32::from_rgb(248, 113, 113)),
            };
            ui.label(RichText::new(format!("PHP {} — {}", r.build.display(), text)).size(10.5).color(color));
        }
        ui.horizontal(|ui| {
            ui.add_enabled_ui(!busy, |ui| {
                if ui.small_button("🔍 Check for updates").clicked() {
                    self.state.php_updater.check(bin_dir.clone());
                }
                if reports.iter().any(|r| matches!(r.status, Status::Outdated(_)))
                    && ui.small_button("⬇ Update all").clicked()
                {
                    self.state.php_updater.update_all(bin_dir.clone());
                }
            });
            if busy {
                ui.spinner();
                ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
            }
        });
        if !message.is_empty() {
            ui.label(RichText::new(message).size(10.0).color(Color32::from_rgb(148, 163, 184)));
        }
    }

    fn render_logs(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Log");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let loading = self.state.is_loading("clear_logs");
                let text = if loading { "⏳ Clearing…" } else { "Clear Logs" };
                if ui.add_enabled(!loading, egui::Button::new(text)).clicked() {
                    self.state.set_loading("clear_logs", 400);
                    self.state.logs.clear();
                }
            });
        });

        ui.add_space(4.0);

        egui::Frame::none()
            .fill(Color32::from_rgb(15, 23, 42))
            .stroke(Stroke::new(1.0_f32, Color32::from_rgb(45, 55, 72)))
            .rounding(4.0)
            .inner_margin(egui::Margin::same(8.0))
            .show(ui, |ui| {
                egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| {
                    for entry in &self.state.logs {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&entry.timestamp).monospace().size(11.0).color(Color32::from_rgb(100, 116, 139)));
                            let (badge_text, badge_color) = match entry.level {
                                LogLevel::Info => ("[INFO]", Color32::from_rgb(56, 189, 248)),
                                LogLevel::Success => ("[SUCCESS]", Color32::from_rgb(16, 185, 129)),
                                LogLevel::Warning => ("[WARN]", Color32::from_rgb(251, 191, 36)),
                                LogLevel::Error => ("[ERROR]", Color32::from_rgb(239, 68, 68)),
                            };
                            ui.label(RichText::new(badge_text).monospace().size(11.0).color(badge_color).strong());
                            ui.label(RichText::new(&entry.message).size(11.0).color(Color32::from_rgb(226, 232, 240)));
                        });
                    }
                });
            });
    }

    fn render_settings(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Settings");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let loading = self.state.is_loading("save_settings");
                if loading {
                    ui.spinner();
                    ui.add_enabled(false, egui::Button::new(RichText::new("Saving…").strong()));
                } else if ui.button(RichText::new("Save Settings").strong()).clicked() {
                    self.state.set_loading("save_settings", 600);
                    self.state.save_settings();
                }
            });
        });
        ui.label(RichText::new(format!("Saved to {}. Ports apply on save; running MySQL / Redis restart on a port change.", crate::settings::path().display()))
            .color(Color32::from_rgb(148, 163, 184)));

        ui.add_space(8.0);

        fn port(ui: &mut egui::Ui, label: &str, value: &mut u16) {
            ui.label(RichText::new(label).strong());
            let mut text = value.to_string();
            if ui.add(egui::TextEdit::singleline(&mut text).desired_width(100.0)).changed() {
                if let Ok(p) = text.parse::<u16>() {
                    if p != 0 {
                        *value = p;
                    }
                }
            }
            ui.end_row();
        }

        egui::Grid::new("settings_grid").spacing([12.0, 10.0]).show(ui, |ui| {
            ui.label(RichText::new("Document Root (www):").strong());
            ui.label(RichText::new(&self.state.www_root).monospace())
                .on_hover_text("Always <myCraft folder>\\www: move mycraft.exe to move it.");
            ui.end_row();

            port(ui, "Web Server Port (HTTP):", &mut self.state.web_port);
            port(ui, "Web Server Port (HTTPS):", &mut self.state.https_port);
            port(ui, "MySQL Port:", &mut self.state.mysql_port);
            port(ui, "Redis Port:", &mut self.state.redis_port);
            port(ui, "Mailpit Web Port:", &mut self.state.mailpit_port);
            port(ui, "Mailpit SMTP Port:", &mut self.state.smtp_port);

            ui.label(RichText::new("Auto Virtual Hosts:").strong());
            let suffix = crate::settings::extract_suffix(&self.state.domain);
            ui.checkbox(&mut self.state.auto_vhosts, format!("Add <project>{suffix} to hosts at startup (as admin) and for new Quick Apps"));
            ui.end_row();

            ui.label(RichText::new("Hostname / Domain:").strong());
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.state.domain).desired_width(120.0))
                    .on_hover_text("Hostname pattern or domain extension (e.g. {name}.go, {name}.test, or .local). Default: {name}.go");
                let example = crate::settings::format_hostname(&self.state.domain, "project");
                ui.label(RichText::new(format!("→ http://{example}")).size(11.0).color(Color32::from_rgb(148, 163, 184)));
            });
            ui.end_row();

            ui.label(RichText::new("Preferred Database Tool:").strong());
            egui::ComboBox::from_id_salt("db_tool_pref_combo")
                .selected_text(&self.state.db_tool_preference)
                .width(220.0)
                .show_ui(ui, |ui| {
                    let db_opts = [
                        "HeidiSQL",
                        "PHP Adminer (Web)",
                        "DBeaver",
                        "Navicat",
                        "MySQL Workbench",
                    ];
                    for opt in db_opts {
                        if ui.selectable_label(self.state.db_tool_preference == opt, opt).clicked() {
                            self.state.db_tool_preference = opt.to_string();
                        }
                    }
                });
            ui.end_row();

            ui.label(RichText::new("Preferred Terminal:").strong());
            egui::ComboBox::from_id_salt("term_pref_combo")
                .selected_text(&self.state.terminal_preference)
                .width(220.0)
                .show_ui(ui, |ui| {
                    let term_opts = [
                        "Cmder",
                        "Windows Terminal",
                        "PowerShell",
                        "Command Prompt",
                        "Git Bash",
                    ];
                    for opt in term_opts {
                        if ui.selectable_label(self.state.terminal_preference == opt, opt).clicked() {
                            self.state.terminal_preference = opt.to_string();
                        }
                    }
                });
            ui.end_row();

            ui.label(RichText::new("Start with Windows:").strong());
            ui.checkbox(&mut self.state.auto_start, "Start myCraft when I sign in to Windows (on Save)");
            ui.end_row();

            ui.label(RichText::new("Close Button:").strong());
            ui.checkbox(&mut self.state.close_to_tray, "Hide to system tray (quit from tray → Exit)");
            ui.end_row();

            ui.label(RichText::new("Notifications:").strong());
            ui.checkbox(&mut self.state.notifications, "Show OS notifications (Quick App, AI, services)");
            ui.end_row();
        });

        ui.add_space(10.0);
        ui.separator();
        ui.horizontal(|ui| {
            ui.label(RichText::new("Feature overview:").strong().color(Color32::from_rgb(148, 163, 184)));
            if ui.button("🔍 View Features Matrix").clicked() {
                self.state.features_modal_open = true;
            }
        });
    }

    fn render_features_modal(&mut self, ctx: &egui::Context) {
        egui::Window::new("🔍 myCraft Features Overview")
            .collapsible(false)
            .resizable(true)
            .default_size([520.0, 380.0])
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // ✕ (U+2715) isn't in egui's bundled fonts; ✖ is.
                        if ui.small_button("✖ Close").clicked() {
                            self.state.features_modal_open = false;
                        }
                    });
                });

                ui.label(RichText::new("What myCraft does today:")
                    .size(11.0)
                    .color(Color32::from_rgb(148, 163, 184)));

                ui.separator();

                egui::ScrollArea::vertical().max_height(270.0).show(ui, |ui| {
                    let suffix = crate::settings::extract_suffix(&self.state.domain);
                    let web_desc = format!("• Built-in HTTP server on the Web port (and 80 when free); HTTPS on the HTTPS port.\n• <project>{suffix} serves www\\<project> (its public\\ folder for Laravel-style apps).\n• HTTPS certificate from a local myCraft CA covering every project; one click to trust it.\n• logs\\access.log and logs\\php_error.log.");
                    let quick_desc = format!("• WordPress (wordpress.org zip, SHA-1 checked, database + wp-config.php), Laravel (Composer, database + .env + migrate), Next.js (create-next-app), static HTML.\n• Adds the <name>{suffix} hosts entry (asks for admin rights when needed).");
                    let groups: [(&str, String); 8] = [
                        ("🌐 Web server", web_desc),
                        ("🐘 PHP", "• Each project runs on the PHP it asks for: .php-version pin, else composer.json, else the newest installed.\n• Installs and security updates from windows.php.net (SHA-256 checked); php.ini per build with common extensions; phpinfo() per build.\n• Composer install for Laravel projects.".into()),
                        ("🗄 MySQL / MariaDB", "• Starts the selected bin\\mysql install; data in data\\<version>, settings conf\\my.ini, log logs\\mysql.log.\n• Random root password (copied to the clipboard); opens HeidiSQL, DBeaver, Navicat, Workbench or Adminer.".into()),
                        ("⚡ Redis", "• Starts bin\\redis with conf\\redis.conf, data in data\\redis; FLUSHALL and redis-cli from the Options.".into()),
                        ("🟢 Node.js", "• Installs from nodejs.org (checksum-verified); terminal with that Node first on PATH.".into()),
                        ("📬 Mailpit", "• SMTP sink (default 1025, no auth) and a web inbox (default 8025) with purge and a test email.".into()),
                        ("🚀 Quick App", quick_desc),
                        ("✨ AI assistant", "• Project-aware chat that can run commands with your approval; several providers and keys with failover, round robin or auto size routing; keys encrypted with DPAPI.".into()),
                    ];
                    for (title, body) in groups {
                        ui.group(|ui| {
                            ui.set_width(ui.available_width());
                            ui.label(RichText::new(title).strong().color(Color32::from_rgb(56, 189, 248)));
                            ui.label(RichText::new(body).size(11.0));
                        });
                        ui.add_space(4.0);
                    }
                    ui.label(RichText::new("Downloadable but not run by myCraft yet: PostgreSQL (packages.conf).").size(10.5).color(Color32::from_rgb(148, 163, 184)));
                });

                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("OK, Understood").clicked() {
                        self.state.features_modal_open = false;
                    }
                });
            });
    }

    fn render_toasts(&mut self, ctx: &egui::Context) {
        let now = std::time::Instant::now();
        self.state.toasts.retain(|t| now.duration_since(t.created_at) < t.duration);

        if self.state.toasts.is_empty() {
            return;
        }

        ctx.request_repaint();

        let mut dismissed_id = None;

        egui::Area::new(egui::Id::new("toast_notification_area"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-16.0, -56.0))
            .show(ctx, |ui| {
                ui.with_layout(egui::Layout::bottom_up(egui::Align::Max), |ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(0.0, 6.0);

                    for t in &self.state.toasts {
                        let elapsed = now.duration_since(t.created_at).as_secs_f32();
                        let total = t.duration.as_secs_f32();
                        let remaining = total - elapsed;
                        let alpha = if remaining < 0.4 {
                            (remaining / 0.4).clamp(0.0, 1.0)
                        } else {
                            1.0
                        };

                        let (icon_symbol, border_color, badge_bg) = match t.kind {
                            crate::state::ToastKind::Success => (
                                "✓",
                                Color32::from_rgb(16, 185, 129),
                                Color32::from_rgba_premultiplied(16, 185, 129, (40.0 * alpha) as u8),
                            ),
                            crate::state::ToastKind::Error => (
                                "✕",
                                Color32::from_rgb(239, 68, 68),
                                Color32::from_rgba_premultiplied(239, 68, 68, (40.0 * alpha) as u8),
                            ),
                            crate::state::ToastKind::Warning => (
                                "⚠",
                                Color32::from_rgb(245, 158, 11),
                                Color32::from_rgba_premultiplied(245, 158, 11, (40.0 * alpha) as u8),
                            ),
                            crate::state::ToastKind::Info => (
                                "ℹ",
                                Color32::from_rgb(56, 189, 248),
                                Color32::from_rgba_premultiplied(56, 189, 248, (40.0 * alpha) as u8),
                            ),
                        };

                        let bg_color = Color32::from_rgba_premultiplied(
                            (15.0 * alpha) as u8,
                            (23.0 * alpha) as u8,
                            (42.0 * alpha) as u8,
                            (245.0 * alpha) as u8,
                        );
                        let stroke_color = Color32::from_rgba_premultiplied(
                            border_color.r(),
                            border_color.g(),
                            border_color.b(),
                            (220.0 * alpha) as u8,
                        );

                        egui::Frame::none()
                            .fill(bg_color)
                            .stroke(Stroke::new(1.0_f32, stroke_color))
                            .rounding(6.0)
                            .inner_margin(egui::Margin::symmetric(10.0, 7.0))
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    egui::Frame::none()
                                        .fill(badge_bg)
                                        .rounding(4.0)
                                        .inner_margin(egui::Margin::symmetric(5.0, 2.0))
                                        .show(ui, |ui| {
                                            ui.label(
                                                RichText::new(icon_symbol)
                                                    .strong()
                                                    .size(11.5)
                                                    .color(border_color),
                                            );
                                        });

                                    ui.add_space(3.0);

                                    ui.label(
                                        RichText::new(&t.message)
                                            .strong()
                                            .size(11.0)
                                            .color(Color32::from_rgba_premultiplied(
                                                (241.0 * alpha) as u8,
                                                (245.0 * alpha) as u8,
                                                (249.0 * alpha) as u8,
                                                (255.0 * alpha) as u8,
                                            )),
                                    );

                                    ui.add_space(4.0);

                                    if ui
                                        .add(
                                            egui::Button::new(
                                                RichText::new("✕")
                                                    .size(9.0)
                                                    .color(Color32::from_rgba_premultiplied(
                                                        (148.0 * alpha) as u8,
                                                        (163.0 * alpha) as u8,
                                                        (184.0 * alpha) as u8,
                                                        (255.0 * alpha) as u8,
                                                    )),
                                            )
                                            .frame(false),
                                        )
                                        .on_hover_text("Dismiss")
                                        .clicked()
                                    {
                                        dismissed_id = Some(t.id);
                                    }
                                });
                            });
                    }
                });
            });

        if let Some(id) = dismissed_id {
            self.state.toasts.retain(|t| t.id != id);
        }
    }
}
