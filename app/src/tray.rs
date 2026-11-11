//! System-tray icon with a right-click menu (like Laragon's), and hiding the
//! main window to the tray.
//!
//! Tray clicks arrive on the event-loop thread; they are queued as `TrayCmd`s
//! and run by `MyCraftApp::update`. A hidden (SW_HIDE) window never gets
//! `update` called on Windows, so "in the tray" means minimized with the
//! taskbar button removed: the window stays alive and keeps processing commands.

use crate::state::MyCraftState;
use eframe::egui;
use std::sync::mpsc::{channel, Receiver};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    BringWindowToTop, GetWindowLongPtrW, IsIconic, IsWindowVisible, SetForegroundWindow,
    SetWindowLongPtrW, ShowWindow, GWL_EXSTYLE, SW_HIDE, SW_RESTORE, SW_SHOW, SW_SHOWMINNOACTIVE,
    WS_EX_TOOLWINDOW,
};

#[derive(Clone, Copy)]
pub enum TrayCmd {
    Show,
    ToggleAll,
    Service(usize),
    Web,
    Database,
    Terminal,
    Root,
    Settings,
    Exit,
}

pub struct Tray {
    _icon: TrayIcon,
    rx: Receiver<TrayCmd>,
    toggle_all: MenuItem,
    services: Vec<CheckMenuItem>,
}

impl Tray {
    pub fn new(ctx: &egui::Context, state: &MyCraftState) -> Option<Self> {
        let mut ids: Vec<(MenuId, TrayCmd)> = Vec::new();
        let mut item = |text: &str, cmd| {
            let item = MenuItem::new(text, true, None);
            ids.push((item.id().clone(), cmd));
            item
        };
        let open = item("Open myCraft", TrayCmd::Show);
        let toggle_all = item(toggle_all_text(state.all_running), TrayCmd::ToggleAll);
        let web = item("Web", TrayCmd::Web);
        let database = item("Database", TrayCmd::Database);
        let terminal = item("Terminal", TrayCmd::Terminal);
        let root = item("Root folder", TrayCmd::Root);
        let settings = item("Settings...", TrayCmd::Settings);
        let exit = item("Exit", TrayCmd::Exit);
        let services: Vec<CheckMenuItem> = state
            .services
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let check = CheckMenuItem::new(&s.name, true, s.running, None);
                ids.push((check.id().clone(), TrayCmd::Service(i)));
                check
            })
            .collect();

        let menu = Menu::new();
        let sep = || PredefinedMenuItem::separator();
        menu.append_items(&[&open, &sep(), &web, &database, &terminal, &root, &sep()]).ok()?;
        for s in &services {
            menu.append(s).ok()?;
        }
        menu.append_items(&[&sep(), &toggle_all, &sep(), &settings, &exit]).ok()?;

        let icon = tray_icon::Icon::from_rgba(crate::logo_rgba(32)?, 32, 32).ok()?;
        let tray = TrayIconBuilder::new()
            .with_menu(Box::new(menu))
            .with_menu_on_left_click(false)
            .with_tooltip("myCraft")
            .with_icon(icon)
            .build()
            .ok()?;

        let (tx, rx) = channel();
        let menu_tx = tx.clone();
        let menu_ctx = ctx.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            if let Some((_, cmd)) = ids.iter().find(|(id, _)| *id == e.id) {
                let _ = menu_tx.send(*cmd);
                menu_ctx.request_repaint();
            }
        }));
        let icon_tx = tx.clone();
        let icon_ctx = ctx.clone();
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                let _ = icon_tx.send(TrayCmd::Show);
                icon_ctx.request_repaint();
            }
        }));

        // Listen for activation requests from secondary instances (e.g. user clicks exe while app is in tray)
        if let Some(event_handle) = crate::single_instance::get_show_event() {
            let show_tx = tx.clone();
            let show_ctx = ctx.clone();
            std::thread::spawn(move || {
                use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
                use windows_sys::Win32::System::Threading::{WaitForSingleObject, INFINITE};
                let h = event_handle as windows_sys::Win32::Foundation::HANDLE;
                loop {
                    let res = unsafe { WaitForSingleObject(h, INFINITE) };
                    if res == WAIT_OBJECT_0 {
                        let _ = show_tx.send(TrayCmd::Show);
                        show_ctx.request_repaint();
                    } else {
                        break;
                    }
                }
            });
        }

        Some(Self { _icon: tray, rx, toggle_all, services })
    }

    pub fn poll(&self) -> Vec<TrayCmd> {
        self.rx.try_iter().collect()
    }

    /// Keep the menu's labels and check marks in step with the services.
    pub fn sync(&self, state: &MyCraftState) {
        self.toggle_all.set_text(toggle_all_text(state.all_running));
        for (item, s) in self.services.iter().zip(&state.services) {
            if item.is_checked() != s.running {
                item.set_checked(s.running);
            }
        }
    }
}

fn toggle_all_text(all_running: bool) -> &'static str {
    if all_running {
        "Stop All"
    } else {
        "Start All"
    }
}

/// Minimize and drop the taskbar button (a tool window has none).
pub fn hide_window(hwnd: isize) {
    let h = hwnd as HWND;
    unsafe {
        ShowWindow(h, SW_HIDE);
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        SetWindowLongPtrW(h, GWL_EXSTYLE, ex | WS_EX_TOOLWINDOW as isize);
        ShowWindow(h, SW_SHOWMINNOACTIVE);
    }
}

/// Bring the window back from the tray (or from minimized) and focus it.
pub fn show_window(hwnd: isize) {
    let h = hwnd as HWND;
    unsafe {
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        if ex & WS_EX_TOOLWINDOW as isize != 0 {
            ShowWindow(h, SW_HIDE);
            SetWindowLongPtrW(h, GWL_EXSTYLE, ex & !(WS_EX_TOOLWINDOW as isize));
            ShowWindow(h, SW_RESTORE);
        } else if IsIconic(h) != 0 {
            ShowWindow(h, SW_RESTORE);
        } else if IsWindowVisible(h) == 0 {
            ShowWindow(h, SW_SHOW);
        }
        BringWindowToTop(h);
        SetForegroundWindow(h);
    }
}
