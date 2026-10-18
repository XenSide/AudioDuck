//! The settings window. Reads/writes `Shared`; never touches audio directly.
use std::sync::Arc;
use std::time::Duration;

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, Frame, Id, Layout, Margin, Modal, ProgressBar, RichText,
    ScrollArea, Sense, Slider, Stroke, Ui, ViewportCommand, vec2,
};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{SW_HIDE, ShowWindow};

use crate::engine::Rule;
use crate::{Config, Live, SharedRef, save_config};

const BG: Color32 = Color32::from_rgb(21, 22, 27);
const CARD: Color32 = Color32::from_rgb(30, 32, 39);
const ROW: Color32 = Color32::from_rgb(37, 40, 50);
const BORDER: Color32 = Color32::from_rgb(42, 45, 54);
const INPUT: Color32 = Color32::from_rgb(42, 45, 54);
const HOVER: Color32 = Color32::from_rgb(52, 56, 68);
const TEXT: Color32 = Color32::from_rgb(230, 232, 238);
const MUTED: Color32 = Color32::from_rgb(139, 144, 160);
const ACCENT: Color32 = Color32::from_rgb(crate::ACCENT[0], crate::ACCENT[1], crate::ACCENT[2]);
const DUCKING: Color32 = Color32::from_rgb(crate::DUCKING[0], crate::DUCKING[1], crate::DUCKING[2]);

fn semibold() -> FontFamily {
    FontFamily::Name("semibold".into())
}

fn setup_style(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    // Segoe UI ships with Windows; fall back to egui's bundled fonts if it isn't there.
    for (name, file) in [("segoe", "segoeui.ttf"), ("segoe-sb", "seguisb.ttf")] {
        if let Ok(bytes) = std::fs::read(format!(r"C:\Windows\Fonts\{file}")) {
            fonts.font_data.insert(name.into(), Arc::new(FontData::from_owned(bytes)));
        }
    }
    let base = fonts.families[&FontFamily::Proportional].clone();
    if fonts.font_data.contains_key("segoe") {
        fonts.families.get_mut(&FontFamily::Proportional).unwrap().insert(0, "segoe".into());
    }
    let mut sb = base;
    if fonts.font_data.contains_key("segoe-sb") {
        sb.insert(0, "segoe-sb".into());
    }
    fonts.families.insert(semibold(), sb);
    ctx.set_fonts(fonts);

    let mut v = egui::Visuals::dark();
    v.panel_fill = BG;
    v.window_fill = CARD;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.window_corner_radius = CornerRadius::same(12);
    v.extreme_bg_color = INPUT;
    v.selection.bg_fill = ACCENT;
    v.selection.stroke = Stroke::new(1.0, BG);
    v.override_text_color = Some(TEXT);
    for (w, fill) in [(&mut v.widgets.inactive, INPUT), (&mut v.widgets.hovered, HOVER), (&mut v.widgets.active, HOVER)] {
        w.weak_bg_fill = fill;
        w.bg_fill = fill;
        w.corner_radius = CornerRadius::same(7);
        w.bg_stroke = Stroke::new(1.0, Color32::from_rgb(64, 68, 82)); // also makes checkboxes visible on cards
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    v.slider_trailing_fill = true;
    ctx.set_visuals_of(egui::Theme::Dark, v);
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|s| {
        s.spacing.item_spacing = vec2(8.0, 8.0);
        s.spacing.button_padding = vec2(12.0, 6.0);
        s.spacing.interact_size.y = 26.0;
        for f in s.text_styles.values_mut() {
            f.size += 1.5;
        }
    });
}

fn pretty(app: &str) -> String {
    let s = app.strip_suffix(".exe").unwrap_or(app);
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn toggle(ui: &mut Ui, on: &mut bool) {
    let (rect, mut resp) = ui.allocate_exact_size(vec2(46.0, 26.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
        resp.mark_changed();
    }
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let p = ui.painter();
    p.rect_filled(rect, 13.0, if *on { ACCENT } else { Color32::from_rgb(58, 62, 74) });
    p.circle_filled(egui::pos2(egui::lerp(rect.left() + 13.0..=rect.right() - 13.0, t), rect.center().y), 9.0, Color32::WHITE);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand);
}

fn card(ui: &mut Ui, title: &str, header_right: impl FnOnce(&mut Ui), body: impl FnOnce(&mut Ui)) {
    Frame::new().fill(CARD).stroke(Stroke::new(1.0, BORDER)).corner_radius(12).inner_margin(Margin::same(16)).show(ui, |ui| {
        ui.set_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(RichText::new(title).size(16.0).family(semibold()));
            ui.with_layout(Layout::right_to_left(Align::Center), header_right);
        });
        ui.add_space(4.0);
        body(ui);
    });
}

fn primary_button(ui: &mut Ui, text: &str) -> bool {
    ui.add(egui::Button::new(RichText::new(text).color(BG).family(semibold())).fill(ACCENT)).on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

struct Editor {
    index: Option<usize>,
    enabled: bool,
    sources: Vec<(String, bool)>,
    targets: Vec<(String, bool)>,
    all_others: bool,
    level: f32,
    manual: String,
    error: &'static str,
}

impl Editor {
    fn new(index: Option<usize>, rule: &Rule, live_apps: impl Iterator<Item = String>) -> Self {
        let mut apps: Vec<String> = live_apps.chain(rule.sources.iter().cloned()).chain(rule.targets.iter().cloned()).collect();
        apps.sort();
        apps.dedup();
        let pick = |chosen: &[String]| apps.iter().map(|a| (a.clone(), chosen.contains(a))).collect();
        Self { index, enabled: rule.enabled, sources: pick(&rule.sources), targets: pick(&rule.targets), all_others: rule.all_others, level: rule.level, manual: String::new(), error: "" }
    }

    fn add_manual(&mut self) {
        let mut app = self.manual.trim().to_lowercase();
        if app.is_empty() {
            return;
        }
        if !app.ends_with(".exe") {
            app.push_str(".exe");
        }
        for list in [&mut self.sources, &mut self.targets] {
            if !list.iter().any(|(a, _)| *a == app) {
                list.push((app.clone(), false));
            }
        }
        self.manual.clear();
    }

    fn checked(list: &[(String, bool)]) -> Vec<String> {
        list.iter().filter(|(_, c)| *c).map(|(a, _)| a.clone()).collect()
    }

    /// Returns Some(rule) when the user confirms a valid rule, Some(None)-like via `done` for cancel.
    fn show(&mut self, ctx: &egui::Context) -> (bool, Option<Rule>) {
        let mut result = (false, None);
        let modal = Modal::new(Id::new("rule-editor"))
            .frame(Frame::new().fill(CARD).stroke(Stroke::new(1.0, BORDER)).corner_radius(12).inner_margin(Margin::same(18)))
            .show(ctx, |ui| {
                ui.set_width(540.0);
                ui.label(RichText::new(if self.index.is_some() { "Edit rule" } else { "New rule" }).size(18.0).family(semibold()));
                ui.add_space(4.0);
                ui.columns(2, |cols| {
                    let all_others = self.all_others;
                    let right = if all_others { "…except these" } else { "…lower" }; // with all_others the list is exclusions
                    for (ui, (title, list)) in cols.iter_mut().zip([("When this plays…", &mut self.sources), (right, &mut self.targets)]) {
                        ui.label(RichText::new(title).family(semibold()));
                        Frame::new().fill(BG).corner_radius(8).inner_margin(Margin::same(8)).show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ScrollArea::vertical().id_salt(list as *const _ as usize).max_height(200.0).min_scrolled_height(200.0).show(ui, |ui| {
                                for (app, on) in list.iter_mut() {
                                    ui.checkbox(on, app.as_str());
                                }
                            });
                        });
                    }
                    cols[1].checkbox(&mut self.all_others, "Lower all other apps");
                });
                ui.horizontal(|ui| {
                    let edit = ui.add(egui::TextEdit::singleline(&mut self.manual).hint_text("App not playing yet, e.g. spotify.exe").desired_width(ui.available_width() - 100.0));
                    if ui.button("Add").clicked() || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                        self.add_manual();
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Volume while ducking");
                    let mut pct = (self.level * 100.0).round() as i32;
                    ui.spacing_mut().slider_width = ui.available_width() - 70.0;
                    if ui.add(Slider::new(&mut pct, 0..=100).suffix("%")).changed() {
                        self.level = pct as f32 / 100.0;
                    }
                });
                if !self.error.is_empty() {
                    ui.label(RichText::new(self.error).color(DUCKING));
                }
                ui.add_space(4.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if primary_button(ui, "Save") {
                        let (sources, targets) = (Self::checked(&self.sources), Self::checked(&self.targets));
                        if sources.is_empty() {
                            self.error = "Pick at least one app that triggers ducking.";
                        } else if !self.all_others && targets.is_empty() {
                            self.error = "Pick at least one app to lower.";
                        } else {
                            result = (true, Some(Rule { sources, targets, all_others: self.all_others, level: self.level, enabled: self.enabled }));
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        result = (true, None);
                    }
                });
            });
        if modal.should_close() {
            result.0 = true;
        }
        result
    }
}

pub struct App {
    shared: SharedRef,
    editor: Option<Editor>,
    start_hidden: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext, shared: SharedRef, start_hidden: bool) -> Self {
        setup_style(&cc.egui_ctx);
        if let Ok(h) = cc.window_handle()
            && let RawWindowHandle::Win32(w) = h.as_raw()
        {
            shared.lock().unwrap().hwnd = w.hwnd.get();
        }
        Self { shared, editor: None, start_hidden }
    }

    fn rules(&mut self, ui: &mut Ui, config: &mut Config, live: &Live) {
        let mut new_rule = false;
        card(ui, "Rules", |ui| new_rule = primary_button(ui, "+ New rule"), |ui| {
            if config.rules.is_empty() {
                ui.label(RichText::new("No rules yet. Example: when Discord plays, lower Spotify to 20%.").color(MUTED));
            }
            let mut delete = None;
            for (i, rule) in config.rules.iter_mut().enumerate() {
                Frame::new().fill(ROW).corner_radius(8).inner_margin(Margin { left: 10, right: 6, top: 6, bottom: 6 }).show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut rule.enabled, "").on_hover_text("Turn rule on or off");
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("×").on_hover_text("Delete").clicked() {
                                delete = Some(i);
                            }
                            if ui.button("Edit").clicked() {
                                self.editor = Some(Editor::new(Some(i), rule, live.peaks.iter().map(|(a, _)| a.clone())));
                            }
                            ui.label(RichText::new(format!("{}%", (rule.level * 100.0).round())).color(MUTED));
                            ui.with_layout(Layout::left_to_right(Align::Center).with_main_wrap(true), |ui| {
                                let sources = rule.sources.iter().map(|a| pretty(a)).collect::<Vec<_>>().join(", ");
                                let listed = rule.targets.iter().map(|a| pretty(a)).collect::<Vec<_>>().join(", ");
                                let targets = match (rule.all_others, rule.targets.is_empty()) {
                                    (true, true) => "all other apps".into(),
                                    (true, false) => format!("all other apps except {listed}"),
                                    _ => listed,
                                };
                                ui.label(RichText::new(sources).family(semibold()));
                                ui.label(RichText::new("→").color(MUTED));
                                ui.label(targets);
                            });
                        });
                    });
                });
            }
            if let Some(i) = delete {
                config.rules.remove(i);
            }
        });
        if new_rule {
            self.editor = Some(Editor::new(None, &Rule::default(), live.peaks.iter().map(|(a, _)| a.clone())));
        }
    }

    fn apps(ui: &mut Ui, config: &Config, live: &Live) {
        card(ui, "Apps playing", |_| {}, |ui| {
            if live.peaks.is_empty() {
                ui.label(RichText::new("No app is playing audio.").color(MUTED));
                return;
            }
            let bar_width = (ui.available_width() - 260.0).max(80.0);
            egui::Grid::new("apps").num_columns(3).spacing(vec2(14.0, 10.0)).min_col_width(120.0).show(ui, |ui| {
                for (app, peak) in &live.peaks {
                    // long process names would push the whole window wider than it is
                    let name = pretty(app);
                    if name.chars().count() > 18 {
                        ui.label(format!("{}…", name.chars().take(17).collect::<String>())).on_hover_text(name);
                    } else {
                        ui.label(name);
                    }
                    let loud = *peak > config.settings.threshold; // orange = loud enough to trigger a rule
                    ui.add(ProgressBar::new(*peak).desired_width(bar_width).desired_height(8.0).corner_radius(4).fill(if loud { DUCKING } else { ACCENT }));
                    let g = if config.enabled { live.gains.get(app).copied().unwrap_or(1.0) } else { 1.0 };
                    let vol = live.volumes.get(app).map(|v| format!("vol {}%", (v * g * 100.0).round())).unwrap_or_default();
                    ui.label(RichText::new(if g < 0.999 { format!("↓ {vol}") } else { vol }).color(if g < 0.999 { DUCKING } else { MUTED }));
                    ui.end_row();
                }
            });
        });
    }

    fn settings(ui: &mut Ui, config: &mut Config) {
        card(ui, "Settings", |_| {}, |ui| {
            let s = &mut config.settings;
            egui::Grid::new("settings").num_columns(2).spacing(vec2(16.0, 10.0)).show(ui, |ui| {
                ui.label("Threshold").on_hover_text("Bars turn orange above the threshold");
                let mut pct = (s.threshold * 100.0).round() as i32;
                ui.spacing_mut().slider_width = (ui.available_width() - 60.0).max(120.0);
                if ui.add(Slider::new(&mut pct, 1..=30).suffix("%")).changed() {
                    s.threshold = pct as f32 / 100.0;
                }
                ui.end_row();
                for (label, v) in [("Fade down", &mut s.attack_ms), ("Hold", &mut s.hold_ms), ("Fade up", &mut s.release_ms)] {
                    ui.label(label);
                    ui.add(egui::DragValue::new(v).range(0..=5000).speed(10).suffix(" ms"));
                    ui.end_row();
                }
            });
        });
    }
}

impl eframe::App for App {
    fn clear_color(&self, _: &egui::Visuals) -> [f32; 4] {
        BG.to_normalized_gamma_f32()
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let shared = self.shared.clone();
        let mut sh = shared.lock().unwrap();
        // eframe shows the window after its first frame even if built invisible, so `--tray` hides it on the next one.
        let hide_now = self.start_hidden && ctx.cumulative_frame_nr() >= 1;
        if self.start_hidden && !hide_now {
            ctx.request_repaint();
        }
        if hide_now {
            self.start_hidden = false;
        }
        if hide_now || ctx.input(|i| i.viewport().close_requested()) {
            // Closing the window keeps AudioDuck running in the tray; quit from the tray menu.
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            unsafe {
                let _ = ShowWindow(HWND(sh.hwnd as _), SW_HIDE);
            }
        }
        // Hidden in the tray or minimized: don't keep repainting; showing/restoring triggers a paint.
        if crate::window_shown(sh.hwnd) {
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        let before = sh.config.clone();
        let crate::Shared { config, live, .. } = &mut *sh;
        Frame::new().fill(BG).show(ui, |ui| {
            ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                Frame::new().inner_margin(Margin { left: 20, right: 20, top: 16, bottom: 20 }).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 12.0;
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            ui.label(RichText::new("AudioDuck").size(24.0).family(semibold()));
                            let status = if !config.enabled { "Paused" } else if live.ducking { "Lowering the volume…" } else { "Listening" };
                            ui.label(RichText::new(status).color(if live.ducking && config.enabled { DUCKING } else { MUTED }));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| toggle(ui, &mut config.enabled));
                    });
                    self.rules(ui, config, live);
                    Self::apps(ui, config, live);
                    Self::settings(ui, config);
                });
            });
        });

        if let Some(editor) = &mut self.editor {
            let (done, rule) = editor.show(&ctx);
            if let Some(rule) = rule {
                match editor.index {
                    Some(i) if i < config.rules.len() => config.rules[i] = rule,
                    _ => config.rules.push(rule),
                }
            }
            if done {
                self.editor = None;
            }
        }

        if sh.config != before {
            sh.rev += 1;
            save_config(&sh.config);
        }
    }
}
