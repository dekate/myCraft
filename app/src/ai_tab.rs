//! "AI" tab: project-scoped chat with routed models, plus the provider /
//! routing setup window. Engine and state live in `ai.rs`.

use eframe::egui::{self, Color32, RichText, Stroke};
use egui_commonmark::{CommonMarkCache, CommonMarkViewer};
use std::time::Duration;

use crate::ai::{self, AiChat, Role, RouteMode, Tier, PRESETS};
use crate::state::{LogLevel, MyCraftState};

const MUTED: Color32 = Color32::from_rgb(148, 163, 184);
const ACCENT: Color32 = Color32::from_rgb(56, 189, 248);
const TEXT: Color32 = Color32::from_rgb(226, 232, 240);
const CARD: Color32 = Color32::from_rgb(15, 23, 42);
const BORDER: Color32 = Color32::from_rgb(45, 55, 72);

const PRESET_PROMPTS: &[&str] = &[
    "Explain how this project is structured and how to run it locally.",
    "Find the cause of the latest error in the attached log and suggest a fix.",
    "Write a database migration for the table described in the attached file.",
];

pub fn show(state: &mut MyCraftState, ui: &mut egui::Ui) {
    for (ok, line) in state.ai.poll() {
        state.log(if ok { LogLevel::Success } else { LogLevel::Error }, &line);
    }
    // Replies and model-list fetches finish on worker threads; keep polling.
    if state.ai.is_busy() || state.ai.setup_open {
        ui.ctx().request_repaint_after(Duration::from_millis(150));
    }

    let www_root = state.www_root.clone();
    let mut log_lines: Vec<(LogLevel, String)> = Vec::new();
    let ai = &mut state.ai;

    toolbar(ai, ui, &www_root);
    if ai.show_context {
        context_panel(ai, ui, &mut log_lines);
    }
    if let Some(n) = ai.notice.clone() {
        ui.horizontal(|ui| {
            ui.label(RichText::new(n).size(10.5).color(Color32::from_rgb(251, 191, 36)));
            if ui.small_button("✖").clicked() {
                ai.notice = None;
            }
        });
    }

    egui::TopBottomPanel::bottom("ai_composer")
        .frame(egui::Frame::none().inner_margin(egui::Margin::symmetric(0.0, 4.0)))
        .show_inside(ui, |ui| composer(ai, ui, &mut log_lines));

    egui::CentralPanel::default()
        .frame(egui::Frame::none().fill(CARD).stroke(Stroke::new(1.0_f32, BORDER)).rounding(4.0).inner_margin(egui::Margin::same(8.0)))
        .show_inside(ui, |ui| stream(ai, ui));

    if ai.setup_open {
        if let Some(res) = setup_window(ai, ui.ctx(), &mut log_lines) {
            match res {
                Ok(()) => state.toast_success("AI configuration saved"),
                Err(e) => state.toast_error(e),
            }
        }
    }

    for (level, line) in log_lines {
        state.log(level, &line);
    }
}

fn toolbar(ai: &mut AiChat, ui: &mut egui::Ui, www_root: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new("Project:").strong().size(11.5));
        let current = short_dir(&ai.project, www_root);
        egui::ComboBox::from_id_salt("ai_project")
            .selected_text(RichText::new(current).monospace().size(11.0))
            .width(130.0)
            .show_ui(ui, |ui| {
                for dir in ai::project_dirs(www_root) {
                    let label = short_dir(&dir, www_root);
                    if ui.selectable_label(dir == ai.project, label).clicked() {
                        ai.set_project(dir);
                    }
                }
            });

        ui.label(RichText::new("Model:").strong().size(11.5));
        let selected = match (&ai.config.mode, &ai.config.single) {
            (RouteMode::Single, Some((_, m))) => m.clone(),
            (mode, _) => mode.label().to_string(),
        };
        let singles: Vec<((u32, String), String)> = ai
            .config
            .routes
            .iter()
            .filter(|r| r.enabled && !r.model.is_empty())
            .map(|r| ((r.provider_id, r.model.clone()), ai.config.route_label(r)))
            .collect();
        let mut save = false;
        egui::ComboBox::from_id_salt("ai_route_mode")
            .selected_text(RichText::new(selected).size(11.0).color(ACCENT))
            .width(140.0)
            .show_ui(ui, |ui| {
                for mode in [RouteMode::Failover, RouteMode::RoundRobin, RouteMode::AutoSize] {
                    if ui.selectable_label(ai.config.mode == mode, mode.label()).clicked() {
                        ai.config.mode = mode;
                        save = true;
                    }
                }
                ui.separator();
                for (key, label) in singles {
                    let on = ai.config.mode == RouteMode::Single && ai.config.single.as_ref() == Some(&key);
                    if ui.selectable_label(on, label).clicked() {
                        ai.config.mode = RouteMode::Single;
                        ai.config.single = Some(key);
                        save = true;
                    }
                }
            });
        if save {
            if let Err(e) = ai.config.save() {
                ai.notice = Some(format!("Could not save AI settings: {}", e));
            }
        }

        if ui.button("🔑 Providers").clicked() {
            ai.setup_open = true;
            ai.autofetch_models();
        }
        let ctx_label = format!("📎 Context ({})", ai.context_files.len());
        if ui.selectable_label(ai.show_context, ctx_label).clicked() {
            ai.show_context = !ai.show_context;
        }
        if ui.add_enabled(!ai.messages.is_empty(), egui::Button::new("🗑")).on_hover_text("Clear conversation").clicked() {
            ai.clear();
        }
    });
    ui.add_space(3.0);
}

fn context_panel(ai: &mut AiChat, ui: &mut egui::Ui, log: &mut Vec<(LogLevel, String)>) {
    egui::Frame::none()
        .fill(Color32::from_rgb(20, 28, 46))
        .stroke(Stroke::new(1.0_f32, BORDER))
        .rounding(4.0)
        .inner_margin(egui::Margin::same(6.0))
        .show(ui, |ui| {
            let mut changed = false;
            ui.horizontal_wrapped(|ui| {
                changed |= ui.checkbox(&mut ai.config.auto_context, "Auto project context").on_hover_text("Send the project's file tree and manifests (README, composer.json, package.json…) with every message. .env is never sent.").changed();
                changed |= ui.checkbox(&mut ai.config.allow_commands, "Let AI run commands").on_hover_text("The assistant can run cmd.exe commands in the project folder and read their output.").changed();
                ui.add_enabled_ui(ai.config.allow_commands, |ui| {
                    changed |= ui.checkbox(&mut ai.config.auto_run, "without asking").on_hover_text("Off: you approve each command before it runs.").changed();
                });
            });
            if changed {
                if let Err(e) = ai.config.save() {
                    ai.notice = Some(format!("Could not save AI settings: {}", e));
                }
            }
            ui.separator();
            ui.label(RichText::new("Files sent with every message (whole file, project-relative):").size(10.5).color(MUTED));
            let mut remove = None;
            ui.horizontal_wrapped(|ui| {
                for (i, f) in ai.context_files.iter().enumerate() {
                    let kb = (f.content.len() as f32 / 1024.0).max(0.1);
                    if ui.small_button(RichText::new(format!("{} ({:.1} KB) ✖", f.rel, kb)).monospace().size(10.5)).on_hover_text("Remove").clicked() {
                        remove = Some(i);
                    }
                }
                if ai.context_files.is_empty() {
                    ui.label(RichText::new("none").italics().size(10.5).color(MUTED));
                }
            });
            if let Some(i) = remove {
                ai.context_files.remove(i);
            }
            ui.horizontal(|ui| {
                let edit = ui.add(egui::TextEdit::singleline(&mut ai.attach_input).hint_text("e.g. routes/api.php").desired_width(220.0).font(egui::TextStyle::Monospace));
                let enter = edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.button("➕ Attach").clicked() || enter {
                    match ai.attach() {
                        Ok(rel) => log.push((LogLevel::Info, format!("AI context: attached {}", rel))),
                        Err(e) => ai.notice = Some(e),
                    }
                }
            });
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("Presets:").size(10.5).color(MUTED));
                for p in PRESET_PROMPTS {
                    let short: String = p.chars().take(34).collect();
                    if ui.small_button(format!("⚡ {}…", short)).on_hover_text(*p).clicked() {
                        ai.input = p.to_string();
                    }
                }
            });
        });
    ui.add_space(3.0);
}

fn stream(ai: &mut AiChat, ui: &mut egui::Ui) {
    let avail_width = ui.available_width();
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .stick_to_bottom(true)
        .max_width(avail_width)
        .horizontal_scroll_offset(0.0)
        .show(ui, |ui| {
            ui.set_max_width(avail_width);
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);

            if ai.messages.is_empty() {
                ui.add_space(20.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new("✨ AI Assistant").size(16.0).strong().color(ACCENT));
                    ui.label(RichText::new("Ask about your project. Add your own API keys in 🔑 Providers;\ncombine several keys and models with failover, round robin, or auto size.").size(11.0).color(MUTED));
                });
            }
            for m in &ai.messages {
                if m.role == Role::Tool {
                    command_card(ui, &m.meta, &m.text, &m.time);
                } else {
                    message(ui, m);
                }
                ui.add_space(6.0);
            }
            if let Some(cmd) = ai.pending_approval().map(str::to_string) {
                approval_card(ai, ui, &cmd);
            } else if ai.is_busy() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("⏳ Working…").size(11.0).color(MUTED));
                });
            }
        });
}

/// A command the model ran, with its output.
fn command_card(ui: &mut egui::Ui, command: &str, output: &str, time: &str) {
    egui::Frame::none()
        .fill(Color32::from_rgb(8, 12, 24))
        .stroke(Stroke::new(1.0_f32, BORDER))
        .rounding(3.0)
        .inner_margin(egui::Margin::same(6.0))
        .show(ui, |ui| {
            let max_w = ui.available_width();
            ui.set_max_width(max_w);

            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("💻 ran").strong().size(10.5).color(Color32::from_rgb(16, 185, 129)));
                ui.label(RichText::new(time).monospace().size(10.0).color(MUTED));
            });

            let mut cmd_job = egui::text::LayoutJob::default();
            cmd_job.append(
                &format!("> {}", command),
                0.0,
                egui::TextFormat::simple(
                    egui::TextStyle::Monospace.resolve(ui.style()),
                    ACCENT,
                ),
            );
            cmd_job.wrap.max_width = max_w;
            cmd_job.wrap.break_anywhere = true;
            ui.add(egui::Label::new(cmd_job).selectable(true));

            if !output.trim().is_empty() {
                egui::CollapsingHeader::new(RichText::new("output").size(10.5).color(MUTED))
                    .id_salt((command, time))
                    .show(ui, |ui| {
                        let inner_max_w = ui.available_width();
                        let mut out_job = egui::text::LayoutJob::default();
                        out_job.append(
                            output,
                            0.0,
                            egui::TextFormat::simple(
                                egui::TextStyle::Monospace.resolve(ui.style()),
                                TEXT,
                            ),
                        );
                        out_job.wrap.max_width = inner_max_w;
                        out_job.wrap.break_anywhere = true;
                        ui.add(egui::Label::new(out_job).selectable(true));
                    });
            }
        });
}

fn approval_card(ai: &mut AiChat, ui: &mut egui::Ui, command: &str) {
    egui::Frame::none()
        .fill(Color32::from_rgb(45, 33, 12))
        .stroke(Stroke::new(1.0_f32, Color32::from_rgb(217, 119, 6)))
        .rounding(4.0)
        .inner_margin(egui::Margin::same(8.0))
        .show(ui, |ui| {
            let max_w = ui.available_width();
            ui.set_max_width(max_w);

            let mut prompt_job = egui::text::LayoutJob::default();
            prompt_job.append(
                &format!("The assistant wants to run this in {}:", ai.project),
                0.0,
                egui::TextFormat::simple(
                    egui::FontId::proportional(11.0),
                    Color32::from_rgb(251, 191, 36),
                ),
            );
            prompt_job.wrap.max_width = max_w;
            prompt_job.wrap.break_anywhere = true;
            ui.add(egui::Label::new(prompt_job));

            let mut cmd_job = egui::text::LayoutJob::default();
            cmd_job.append(
                command,
                0.0,
                egui::TextFormat::simple(
                    egui::TextStyle::Monospace.resolve(ui.style()),
                    TEXT,
                ),
            );
            cmd_job.wrap.max_width = max_w;
            cmd_job.wrap.break_anywhere = true;
            ui.add(egui::Label::new(cmd_job).selectable(true));

            ui.horizontal_wrapped(|ui| {
                if ui.button(RichText::new("▶ Run").strong()).clicked() {
                    ai.answer_approval(true);
                }
                if ui.button("✖ Skip").clicked() {
                    ai.answer_approval(false);
                }
                if ui.button("Always allow").on_hover_text("Run future commands without asking (change in 📎 Context)").clicked() {
                    ai.config.auto_run = true;
                    if let Err(e) = ai.config.save() {
                        ai.notice = Some(format!("Could not save AI settings: {}", e));
                    }
                    ai.answer_approval(true);
                }
            });
        });
}

fn message(ui: &mut egui::Ui, m: &ai::ChatMsg) {
    let (who, color, fill) = match m.role {
        Role::User => ("You", Color32::from_rgb(147, 197, 253), Color32::from_rgb(30, 41, 70)),
        Role::Assistant | Role::Tool => ("✨ Assistant", ACCENT, Color32::from_rgb(20, 30, 50)),
        Role::Error => ("⚠ Error", Color32::from_rgb(239, 68, 68), Color32::from_rgb(50, 20, 24)),
    };
    ui.horizontal_wrapped(|ui| {
        ui.label(RichText::new(who).strong().size(11.5).color(color));
        ui.label(RichText::new(&m.time).monospace().size(10.0).color(MUTED));
        if !m.meta.is_empty() {
            ui.add(egui::Label::new(RichText::new(&m.meta).monospace().size(10.0).color(MUTED)).wrap());
        }
    });
    egui::Frame::none().fill(fill).rounding(4.0).inner_margin(egui::Margin::same(7.0)).show(ui, |ui| {
        let max_w = ui.available_width();
        ui.set_max_width(max_w);
        body(ui, &m.role, &m.text);
    });
}

/// Assistant replies are markdown; user text is shown as typed.
fn body(ui: &mut egui::Ui, role: &Role, text: &str) {
    if matches!(role, Role::User) {
        let mut job = egui::text::LayoutJob::default();
        job.append(
            text,
            0.0,
            egui::TextFormat::simple(
                egui::FontId::proportional(12.0),
                TEXT,
            ),
        );
        job.wrap.max_width = ui.available_width();
        job.wrap.break_anywhere = true;
        ui.add(egui::Label::new(job).selectable(true));
        return;
    }
    // One cache for the UI thread: holds syntax-highlighting state and table /
    // code-block layout between frames.
    thread_local! {
        static CACHE: std::cell::RefCell<CommonMarkCache> = std::cell::RefCell::new(CommonMarkCache::default());
    }
    CACHE.with(|cache| {
        CommonMarkViewer::new()
            .syntax_theme_dark("base16-ocean.dark")
            .show(ui, &mut cache.borrow_mut(), text);
    });
}

fn composer(ai: &mut AiChat, ui: &mut egui::Ui, log: &mut Vec<(LogLevel, String)>) {
    let edit = ui.add(
        egui::TextEdit::multiline(&mut ai.input)
            .hint_text("Ask about your code, logs, or setup…  (Ctrl+Enter to send)")
            .desired_rows(2)
            .desired_width(f32::INFINITY),
    );
    let ctrl_enter = edit.has_focus() && ui.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::Enter));
    ui.horizontal_wrapped(|ui| {
        let can_send = !ai.is_busy() && !ai.input.trim().is_empty();
        let send = ui.add_enabled(can_send, egui::Button::new(RichText::new("Send ➡").strong()).fill(Color32::from_rgb(8, 145, 178)));
        if ai.is_busy() && ui.button("⏹ Stop").clicked() {
            ai.stop();
            log.push((LogLevel::Warning, "AI request stopped by user.".into()));
        }
        ui.add(egui::Label::new(RichText::new(ai.preview()).size(10.0).color(MUTED)).wrap());
        if (send.clicked() || ctrl_enter) && can_send {
            match ai.send() {
                Ok(line) => log.push((LogLevel::Info, line)),
                Err(e) => ai.notice = Some(e),
            }
        }
    });
}

enum ProviderAction {
    Fetch(u32),
    Remove(u32),
}

enum RouteAction {
    Up(usize),
    Down(usize),
    Remove(usize),
}

fn setup_window(ai: &mut AiChat, ctx: &egui::Context, log: &mut Vec<(LogLevel, String)>) -> Option<Result<(), String>> {
    let mut open = true;
    let mut save_clicked = false;
    egui::Window::new("🔑 AI Providers & Routing")
        .open(&mut open)
        .collapsible(false)
        .default_size([500.0, 380.0])
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading(RichText::new("Providers (API keys)").size(14.0));
                ui.label(RichText::new("Add as many keys as you like, even several for one vendor. Keys are encrypted with Windows DPAPI for your user account.").size(10.5).color(MUTED));
                ui.add_space(4.0);

                let mut p_action = None;
                let fetching_ids: Vec<u32> = ai.config.providers.iter().map(|p| p.id).filter(|&id| ai.is_fetching(id)).collect();
                for p in ai.config.providers.iter_mut() {
                    let fetching = fetching_ids.contains(&p.id);
                    let model_count = ai.models.get(&p.id).map(|m| m.len());
                    egui::Frame::none().stroke(Stroke::new(1.0_f32, BORDER)).rounding(4.0).inner_margin(egui::Margin::same(6.0)).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.checkbox(&mut p.enabled, "");
                            ui.add(egui::TextEdit::singleline(&mut p.label).desired_width(140.0));
                            ui.label(RichText::new(match p.wire { ai::Wire::Anthropic => "Anthropic API", ai::Wire::OpenAi => "OpenAI-compatible" }).size(10.0).color(MUTED));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if ui.small_button("🗑").on_hover_text("Remove provider and its routes").clicked() {
                                    p_action = Some(ProviderAction::Remove(p.id));
                                }
                                if fetching {
                                    ui.spinner();
                                } else if ui.small_button("⟳ Test / models").on_hover_text("Check the key and load the model list").clicked() {
                                    p_action = Some(ProviderAction::Fetch(p.id));
                                }
                                if let Some(n) = model_count {
                                    ui.label(RichText::new(format!("✔ {} models", n)).size(10.0).color(Color32::from_rgb(16, 185, 129)));
                                }
                            });
                        });
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("URL").size(10.5).color(MUTED));
                            ui.add(egui::TextEdit::singleline(&mut p.base_url).desired_width(f32::INFINITY).font(egui::TextStyle::Monospace));
                        });
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Key").size(10.5).color(MUTED));
                            ui.add(egui::TextEdit::singleline(&mut p.api_key).password(true).hint_text("paste API key (blank for local servers)").desired_width(f32::INFINITY));
                        });
                    });
                    ui.add_space(3.0);
                }
                match p_action {
                    Some(ProviderAction::Fetch(id)) => ai.fetch_models(id),
                    Some(ProviderAction::Remove(id)) => {
                        ai.config.remove_provider(id);
                        ai.models.remove(&id);
                    }
                    None => {}
                }

                ui.horizontal(|ui| {
                    egui::ComboBox::from_id_salt("ai_add_preset")
                        .selected_text(PRESETS[ai.add_preset_idx].name)
                        .width(200.0)
                        .show_ui(ui, |ui| {
                            for (i, p) in PRESETS.iter().enumerate() {
                                ui.selectable_value(&mut ai.add_preset_idx, i, p.name);
                            }
                        });
                    if ui.button("➕ Add provider").clicked() {
                        let preset = &PRESETS[ai.add_preset_idx];
                        ai.config.add_preset(preset);
                        let key_step = if preset.needs_key { "paste the API key, " } else { "" };
                        ai.notice = Some(if preset.small.is_empty() {
                            format!("{} added: {}press ⟳ Test / models, then ➕ Add route for the models you want.", preset.name, key_step)
                        } else {
                            format!("{} added with default small/big routes: {}then ⟳ Test / models.", preset.name, key_step)
                        });
                    }
                });

                ui.add_space(8.0);
                ui.separator();
                ui.heading(RichText::new("Routing").size(14.0));
                for (mode, help) in [
                    (RouteMode::Failover, "Try routes top to bottom; on any error or rate limit, move to the next."),
                    (RouteMode::RoundRobin, "Start from a different route each message to spread load across keys; still fails over."),
                    (RouteMode::AutoSize, "Short, simple prompts go to Small routes, big tasks to Big routes; the other tier is the fallback."),
                ] {
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut ai.config.mode, mode, mode.label());
                        ui.label(RichText::new(help).size(10.0).color(MUTED));
                    });
                }
                ui.label(RichText::new("A single model can be picked from the Model dropdown in the chat bar. Rate-limited routes (429/529) are moved to the back for 60s.").size(10.0).color(MUTED));

                ui.add_space(6.0);
                ui.label(RichText::new("Routes (priority order)").strong());
                let providers: Vec<(u32, String)> = ai.config.providers.iter().map(|p| (p.id, p.label.clone())).collect();
                let mut r_action = None;
                let n_routes = ai.config.routes.len();
                for i in 0..n_routes {
                    let cooling = ai.cooling_secs(&ai.config.routes[i]);
                    let models = ai.models.get(&ai.config.routes[i].provider_id).cloned().unwrap_or_default();
                    let r = &mut ai.config.routes[i];
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{}.", i + 1)).monospace().size(10.5).color(MUTED));
                        ui.checkbox(&mut r.enabled, "");
                        let plabel = providers.iter().find(|(id, _)| *id == r.provider_id).map(|(_, l)| l.as_str()).unwrap_or("?");
                        egui::ComboBox::from_id_salt(("ai_route_provider", i)).selected_text(plabel).width(95.0).show_ui(ui, |ui| {
                            for (id, label) in &providers {
                                ui.selectable_value(&mut r.provider_id, *id, label);
                            }
                        });
                        if models.is_empty() {
                            ui.add(egui::TextEdit::singleline(&mut r.model).hint_text("model id (⟳ Test to list)").desired_width(150.0).font(egui::TextStyle::Monospace));
                        } else {
                            // Fetched list as a dropdown; the text box at the top
                            // still allows ids the provider doesn't list.
                            let shown = if r.model.is_empty() { "pick a model".to_string() } else { r.model.clone() };
                            egui::ComboBox::from_id_salt(("ai_route_model", i))
                                .selected_text(RichText::new(shown).monospace().size(11.0))
                                .width(170.0)
                                .height(260.0)
                                .show_ui(ui, |ui| {
                                    ui.add(egui::TextEdit::singleline(&mut r.model).hint_text("custom model id").font(egui::TextStyle::Monospace));
                                    ui.separator();
                                    for m in &models {
                                        ui.selectable_value(&mut r.model, m.clone(), RichText::new(m).monospace().size(11.0));
                                    }
                                });
                        }
                        egui::ComboBox::from_id_salt(("ai_route_tier", i)).selected_text(r.tier.label()).width(52.0).show_ui(ui, |ui| {
                            ui.selectable_value(&mut r.tier, Tier::Small, "small");
                            ui.selectable_value(&mut r.tier, Tier::Big, "big");
                        });
                        if ui.add_enabled(i > 0, egui::Button::new("⬆").small()).clicked() {
                            r_action = Some(RouteAction::Up(i));
                        }
                        if ui.add_enabled(i + 1 < n_routes, egui::Button::new("⬇").small()).clicked() {
                            r_action = Some(RouteAction::Down(i));
                        }
                        if ui.small_button("🗑").clicked() {
                            r_action = Some(RouteAction::Remove(i));
                        }
                        if let Some(s) = cooling {
                            ui.label(RichText::new(format!("rate-limited {}s", s)).size(10.0).color(Color32::from_rgb(251, 191, 36)));
                        }
                    });
                }
                match r_action {
                    Some(RouteAction::Up(i)) => ai.config.routes.swap(i, i - 1),
                    Some(RouteAction::Down(i)) => ai.config.routes.swap(i, i + 1),
                    Some(RouteAction::Remove(i)) => {
                        ai.config.routes.remove(i);
                    }
                    None => {}
                }
                if let Some((id, _)) = providers.first() {
                    if ui.button("➕ Add route").clicked() {
                        ai.config.routes.push(ai::Route { provider_id: *id, model: String::new(), tier: Tier::Big, enabled: true });
                    }
                } else {
                    ui.label(RichText::new("Add a provider first.").italics().size(10.5).color(MUTED));
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    if ui.button(RichText::new("💾 Save").strong()).clicked() {
                        save_clicked = true;
                    }
                    ui.label(RichText::new(ai::config_path().display().to_string()).monospace().size(10.0).color(MUTED));
                });
            });
        });

    let mut result = None;
    // Closing the window saves too, so edits are never silently lost.
    if save_clicked || !open {
        match ai.config.save() {
            Ok(()) => {
                log.push((LogLevel::Success, "AI provider settings saved.".into()));
                if save_clicked {
                    result = Some(Ok(()));
                }
            }
            Err(e) => {
                let msg = format!("Could not save AI settings: {}", e);
                ai.notice = Some(msg.clone());
                if save_clicked {
                    result = Some(Err(msg));
                }
            }
        }
    }
    if !open {
        ai.setup_open = false;
    }
    result
}

/// Folder name relative to the www root, for compact display.
fn short_dir(dir: &str, www_root: &str) -> String {
    if dir == www_root {
        "(www root)".into()
    } else {
        std::path::Path::new(dir).file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| dir.to_string())
    }
}
