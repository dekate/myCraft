// Release builds are GUI-subsystem: no console window opens next to the app.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
mod ai_tab;
mod app;
mod mysql;
pub mod notify;
mod packages;
mod php_update;
mod quickapp;
mod redis;
mod server;
mod settings;
mod single_instance;
mod ssl;
mod state;
#[cfg(windows)]
mod tray;
pub mod tunnel;

use app::MyCraftApp;
use eframe::egui;
use std::env;
use std::path::PathBuf;

fn main() -> eframe::Result<()> {
    let args: Vec<String> = env::args().collect();
    let mut screenshot_target = None;
    let mut i = 1;
    while i < args.len() {
        if args[i] == "--screenshot" && i + 1 < args.len() {
            screenshot_target = Some(PathBuf::from(&args[i + 1]));
            i += 2;
        } else {
            i += 1;
        }
    }

    #[cfg(windows)]
    if screenshot_target.is_none() {
        if let single_instance::InstanceStatus::AlreadyRunning = single_instance::acquire_or_activate() {
            return Ok(());
        }
    }

    ensure_layout();
    let mut app = MyCraftApp::new(screenshot_target);

    // If running in screenshot mode, start services so the screenshot reflects the running dev box state
    if app.screenshot_target.is_some() {
        app.state.start_all();
    }

    // Launch background HTTP server
    server::start_http_server(
        app.state.server_running_flag.clone(),
        app.state.www_root.clone(),
        app.state.tunnel_manager.clone(),
    );

    // Launch background Mailpit Web & SMTP server
    server::start_mailpit_server(app.state.mailpit_running_flag.clone());

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([540.0, 450.0])
        .with_min_inner_size([480.0, 380.0])
        .with_title("Mycraft - Full Stack Dev Box");

    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }

    let native_options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    eframe::run_native(
        "Mycraft",
        native_options,
        Box::new(move |cc| {
            cc.egui_ctx.set_fonts(fonts());
            // The tray icon must be created on the UI thread, inside the event loop.
            #[cfg(windows)]
            if app.screenshot_target.is_none() {
                app.attach_tray(cc);
            }
            Ok(Box::new(app))
        }),
    )
}

/// Fonts are compiled into the exe, so the UI looks the same offline and on
/// machines without them installed. Inter replaces egui's Ubuntu-Light for UI
/// text; Inter SemiBold is the "bold" family markdown uses; Noto Sans Symbols 2
/// and Hack catch symbols (✓ ☐ → ▼ box drawing) the others lack.
fn fonts() -> egui::FontDefinitions {
    use egui::{FontData, FontFamily};
    let mut fonts = egui::FontDefinitions::default();
    let mut add = |name: &str, bytes: &'static [u8]| {
        fonts.font_data.insert(name.to_owned(), std::sync::Arc::new(FontData::from_static(bytes)));
    };
    add("Inter", include_bytes!("../assets/fonts/Inter-Regular.ttf"));
    add("Inter-SemiBold", include_bytes!("../assets/fonts/Inter-SemiBold.ttf"));
    add("NotoSansSymbols2", include_bytes!("../assets/fonts/NotoSansSymbols2-Regular.ttf"));

    // Default proportional list is [Ubuntu-Light, NotoEmoji, emoji-icon-font].
    let emoji: Vec<String> = fonts.families[&FontFamily::Proportional].iter().filter(|f| *f != "Ubuntu-Light").cloned().collect();
    let tail = [emoji, vec!["NotoSansSymbols2".to_owned(), "Hack".to_owned()]].concat();
    let family = |first: &str| [vec![first.to_owned()], tail.clone()].concat();
    fonts.families.insert(FontFamily::Proportional, family("Inter"));
    fonts.families.insert(FontFamily::Name("bold".into()) /* = vendored BOLD_FAMILY */, family("Inter-SemiBold"));
    if let Some(mono) = fonts.families.get_mut(&FontFamily::Monospace) {
        mono.push("NotoSansSymbols2".to_owned());
    }
    fonts
}

/// The myCraft root: the folder holding mycraft.exe (`D:\mycraft\mycraft.exe`
/// → `D:\mycraft`), with `www\` for projects and `bin\php\` for PHP builds.
/// `cargo run` puts the exe in `target\debug`, so a dev build uses an existing
/// `G:\LARAGON` or `C:\laragon` box instead when there is one.
pub fn mycraft_root() -> &'static std::path::Path {
    static ROOT: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    ROOT.get_or_init(|| {
        let exe_dir = env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(PathBuf::from))
            .unwrap_or_else(|| PathBuf::from("."));
        let in_cargo_target = exe_dir.parent().and_then(|p| p.file_name()).is_some_and(|n| n == "target");
        if in_cargo_target {
            if let Some(dev) = ["G:\\LARAGON", "C:\\laragon"].map(PathBuf::from).into_iter().find(|p| p.join("www").is_dir()) {
                return dev;
            }
        }
        exe_dir
    })
}

/// Create the folders a fresh install needs, so users know where projects go.
fn ensure_layout() {
    let root = mycraft_root();
    for dir in ["www", "bin\\php", "conf", "etc\\ssl", "usr"] {
        let _ = std::fs::create_dir_all(root.join(dir));
    }
    packages::load(); // writes the default usr\packages.conf if missing
}

/// Run a console tool (cmd, powershell, php-cgi, certutil) without popping a
/// console window — a GUI-subsystem app has no console for children to share.
pub fn hide_console(cmd: &mut std::process::Command) -> &mut std::process::Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// Title-bar/taskbar icon: logo padded to a square and scaled to 256px,
/// since Windows rejects or mangles large non-square window icons.
fn window_icon() -> Option<egui::IconData> {
    const SIZE: u32 = 256;
    Some(egui::IconData { rgba: logo_rgba(SIZE)?, width: SIZE, height: SIZE })
}

/// The logo as a `size`×`size` RGBA square, for the window and tray icons.
pub fn logo_rgba(size: u32) -> Option<Vec<u8>> {
    use image::imageops;
    let logo = image::load_from_memory(include_bytes!("../assets/logo.png")).ok()?.to_rgba8();
    let side = logo.width().max(logo.height());
    let mut square = image::RgbaImage::new(side, side);
    let x = (side - logo.width()) / 2;
    let y = (side - logo.height()) / 2;
    imageops::overlay(&mut square, &logo, x as i64, y as i64);
    Some(imageops::resize(&square, size, size, imageops::FilterType::Lanczos3).into_raw())
}
