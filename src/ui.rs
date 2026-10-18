//! The settings window. Reads/writes `Shared`; never touches audio directly.
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use slint::{ComponentHandle, Image, Model, ModelRc, Rgba8Pixel, SharedPixelBuffer, Timer, TimerMode, VecModel};

use crate::engine::Rule;
use crate::{Config, SharedRef, save_config};

slint::include_modules!();

fn pretty(app: &str) -> String {
    let s = app.strip_suffix(".exe").unwrap_or(app);
    let mut c = s.chars();
    c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default()
}

fn rule_row(rule: &Rule) -> RuleRow {
    let sources = rule.sources.iter().map(|a| pretty(a)).collect::<Vec<_>>().join(", ");
    let listed = rule.targets.iter().map(|a| pretty(a)).collect::<Vec<_>>().join(", ");
    let targets = match (rule.all_others, rule.targets.is_empty()) {
        (true, true) => "all other apps".into(),
        (true, false) => format!("all other apps except {listed}"),
        _ => listed,
    };
    RuleRow { enabled: rule.enabled, sources: sources.into(), targets: targets.into(), level: format!("{}%", (rule.level * 100.0).round()).into() }
}

fn push_config(w: &MainWindow, c: &Config) {
    w.set_enabled(c.enabled);
    w.set_rules(ModelRc::new(VecModel::from(c.rules.iter().map(rule_row).collect::<Vec<_>>())));
    w.set_threshold((c.settings.threshold * 100.0).round());
    w.set_fade_down(c.settings.attack_ms as i32);
    w.set_hold(c.settings.hold_ms as i32);
    w.set_fade_up(c.settings.release_ms as i32);
}

/// Brings the window up from the tray. Call on the UI thread.
pub fn bring_up(w: &MainWindow) {
    let _ = w.show();
    w.window().set_minimized(false);
    if let Ok(h) = w.window().window_handle().window_handle()
        && let RawWindowHandle::Win32(h) = h.as_raw()
    {
        crate::show_window(h.hwnd.get());
    }
}

struct Editor {
    index: Option<usize>,
    enabled: bool,
    sources: Vec<(String, bool)>,
    targets: Vec<(String, bool)>,
}

impl Editor {
    fn open(w: &MainWindow, index: Option<usize>, rule: &Rule, live_apps: impl Iterator<Item = String>) -> Self {
        let mut apps: Vec<String> = live_apps.chain(rule.sources.iter().cloned()).chain(rule.targets.iter().cloned()).collect();
        apps.sort();
        apps.dedup();
        let pick = |chosen: &[String]| apps.iter().map(|a| (a.clone(), chosen.contains(a))).collect();
        let ed = Self { index, enabled: rule.enabled, sources: pick(&rule.sources), targets: pick(&rule.targets) };
        ed.push_lists(w);
        w.set_editor_title(if index.is_some() { "Edit rule" } else { "New rule" }.into());
        w.set_all_others(rule.all_others);
        w.set_level((rule.level * 100.0).round());
        w.set_manual("".into());
        w.set_editor_error("".into());
        w.set_editor_open(true);
        ed
    }

    fn push_lists(&self, w: &MainWindow) {
        let picks = |list: &[(String, bool)]| ModelRc::new(VecModel::from(list.iter().map(|(a, c)| Pick { name: a.into(), checked: *c }).collect::<Vec<_>>()));
        w.set_sources(picks(&self.sources));
        w.set_targets(picks(&self.targets));
    }

    fn add_manual(&mut self, w: &MainWindow) {
        let mut app = w.get_manual().trim().to_lowercase();
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
        w.set_manual("".into());
        self.push_lists(w);
    }

    fn checked(list: &[(String, bool)]) -> Vec<String> {
        list.iter().filter(|(_, c)| *c).map(|(a, _)| a.clone()).collect()
    }

    /// The rule to save, or the reason it can't be saved yet.
    fn rule(&self, w: &MainWindow) -> Result<Rule, &'static str> {
        let (sources, targets, all_others) = (Self::checked(&self.sources), Self::checked(&self.targets), w.get_all_others());
        if sources.is_empty() {
            Err("Pick at least one app that triggers ducking.")
        } else if !all_others && targets.is_empty() {
            Err("Pick at least one app to lower.")
        } else {
            Ok(Rule { sources, targets, all_others, level: w.get_level() / 100.0, enabled: self.enabled })
        }
    }
}

type Change = dyn Fn(&dyn Fn(&mut Config));

/// Builds the window, starts the engine via `spawn_engine`, and runs until Quit in the tray.
pub fn run(shared: SharedRef, start_hidden: bool, spawn_engine: impl FnOnce(slint::Weak<MainWindow>)) -> Result<(), slint::PlatformError> {
    let w = MainWindow::new()?;
    let icon = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&crate::icon_rgba(crate::ACCENT, 64), 64, 64);
    w.set_app_icon(Image::from_rgba8(icon));
    let pushed = Rc::new(Cell::new(u64::MAX)); // config rev the window last showed
    let editor: Rc<RefCell<Option<Editor>>> = Rc::default();

    // Every edit goes through here: change the config, persist it, and redraw from it.
    let change: Rc<Change> = {
        let (shared, weak, pushed) = (shared.clone(), w.as_weak(), pushed.clone());
        Rc::new(move |f| {
            let mut sh = shared.lock().unwrap();
            f(&mut sh.config);
            sh.rev += 1;
            pushed.set(sh.rev);
            save_config(&sh.config);
            if let Some(w) = weak.upgrade() {
                push_config(&w, &sh.config);
            }
        })
    };
    let live_apps = {
        let shared = shared.clone();
        move || shared.lock().unwrap().live.peaks.iter().map(|(a, _)| a.clone()).collect::<Vec<_>>()
    };

    let c = change.clone();
    w.on_enabled_toggled(move |on| c(&|cfg| cfg.enabled = on));
    let c = change.clone();
    w.on_rule_toggled(move |i, on| c(&|cfg| if let Some(r) = cfg.rules.get_mut(i as usize) { r.enabled = on }));
    let c = change.clone();
    w.on_delete_rule(move |i| c(&|cfg| if (i as usize) < cfg.rules.len() { cfg.rules.remove(i as usize); }));
    let c = change.clone();
    let weak = w.as_weak();
    w.on_settings_changed(move || {
        let w = weak.unwrap();
        let (t, d, h, u) = (w.get_threshold(), w.get_fade_down(), w.get_hold(), w.get_fade_up());
        c(&|cfg| {
            let s = &mut cfg.settings;
            s.threshold = t.round() / 100.0;
            (s.attack_ms, s.hold_ms, s.release_ms) = (d.max(0) as u32, h.max(0) as u32, u.max(0) as u32);
        })
    });

    let (weak, ed, apps) = (w.as_weak(), editor.clone(), live_apps.clone());
    w.on_new_rule(move || {
        *ed.borrow_mut() = Some(Editor::open(&weak.unwrap(), None, &Rule::default(), apps().into_iter()));
    });
    let (weak, ed, sh, apps) = (w.as_weak(), editor.clone(), shared.clone(), live_apps.clone());
    w.on_edit_rule(move |i| {
        let rule = sh.lock().unwrap().config.rules.get(i as usize).cloned();
        if let Some(rule) = rule {
            *ed.borrow_mut() = Some(Editor::open(&weak.unwrap(), Some(i as usize), &rule, apps().into_iter()));
        }
    });
    let ed = editor.clone();
    w.on_pick(move |target, i, on| {
        if let Some(e) = ed.borrow_mut().as_mut() {
            let list = if target { &mut e.targets } else { &mut e.sources };
            if let Some(p) = list.get_mut(i as usize) {
                p.1 = on;
            }
        }
    });
    let (weak, ed) = (w.as_weak(), editor.clone());
    w.on_add_manual(move || {
        if let Some(e) = ed.borrow_mut().as_mut() {
            e.add_manual(&weak.unwrap());
        }
    });
    let (weak, ed, c) = (w.as_weak(), editor.clone(), change.clone());
    w.on_save(move || {
        let w = weak.unwrap();
        let Some(e) = ed.borrow_mut().take() else { return };
        match e.rule(&w) {
            Err(msg) => {
                w.set_editor_error(msg.into());
                *ed.borrow_mut() = Some(e);
            }
            Ok(rule) => {
                let index = e.index;
                c(&|cfg| match index {
                    Some(i) if i < cfg.rules.len() => cfg.rules[i] = rule.clone(),
                    _ => cfg.rules.push(rule.clone()),
                });
                w.set_editor_open(false);
            }
        }
    });
    let (weak, ed) = (w.as_weak(), editor.clone());
    w.on_cancel(move || {
        ed.borrow_mut().take();
        weak.unwrap().set_editor_open(false);
    });

    // Live meters and changes from the tray. Slint only repaints what these setters actually change.
    let apps_model = Rc::new(VecModel::<AppRow>::default());
    w.set_apps(ModelRc::from(apps_model.clone()));
    let timer = Timer::default();
    let (weak, sh) = (w.as_weak(), shared.clone());
    timer.start(TimerMode::Repeated, Duration::from_millis(50), move || {
        let Some(w) = weak.upgrade() else { return };
        let shown = w.window().is_visible() && !w.window().is_minimized();
        let mut sh = sh.lock().unwrap();
        sh.visible = shown;
        if !shown {
            return;
        }
        if sh.rev != pushed.get() {
            pushed.set(sh.rev);
            push_config(&w, &sh.config);
        }
        let (c, live) = (&sh.config, &sh.live);
        w.set_status(if !c.enabled { "Paused" } else if live.ducking { "Lowering the volume…" } else { "Listening" }.into());
        w.set_lowering(c.enabled && live.ducking);
        let rows: Vec<AppRow> = live
            .peaks
            .iter()
            .map(|(app, peak)| {
                let g = if c.enabled { live.gains.get(app).copied().unwrap_or(1.0) } else { 1.0 };
                let vol = live.volumes.get(app).map(|v| format!("vol {}%", (v * g * 100.0).round())).unwrap_or_default();
                let lowered = g < 0.999;
                AppRow { name: pretty(app).into(), peak: *peak, loud: *peak > c.settings.threshold, vol: if lowered { format!("↓ {vol}") } else { vol }.into(), lowered }
            })
            .collect();
        if rows.len() != apps_model.row_count() {
            apps_model.set_vec(rows);
        } else {
            for (i, row) in rows.into_iter().enumerate() {
                if apps_model.row_data(i).as_ref() != Some(&row) {
                    apps_model.set_row_data(i, row);
                }
            }
        }
    });

    spawn_engine(w.as_weak());
    if !start_hidden {
        w.show()?;
    }
    // Closing the window only hides it (Slint's default); AudioDuck keeps running in the tray until Quit.
    slint::run_event_loop_until_quit()
}
