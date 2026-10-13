//! Pure ducking logic: peaks in, per-app gains out. No Windows here so it can be tested.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rule {
    /// Stable across edits and reorders, so a Stream Deck key can point at this rule. 0 = not assigned yet.
    pub id: u64,
    pub sources: Vec<String>,
    /// Apps to duck; with `all_others`, apps to leave alone.
    pub targets: Vec<String>,
    /// Duck every app except the sources (and `targets`).
    #[serde(alias = "duck_all")]
    pub all_others: bool,
    /// Target volume while ducked, relative to the user's volume.
    #[serde(alias = "duck_amount")]
    pub level: f32,
    pub enabled: bool,
}

impl Default for Rule {
    fn default() -> Self {
        Self { id: 0, sources: vec![], targets: vec![], all_others: false, level: 0.2, enabled: true }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Source peak (real process output via loopback) above this counts as "talking".
    pub threshold: f32,
    pub attack_ms: u32,
    /// Keep ducking this long after the source goes quiet (gaps between words).
    pub hold_ms: u32,
    pub release_ms: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { threshold: 0.05, attack_ms: 150, hold_ms: 600, release_ms: 900 }
    }
}

#[derive(Default)]
pub struct Ducker {
    pub gains: HashMap<String, f32>,
    /// Per rule (same order as `rules`): whether it is ducking right now.
    pub active: Vec<bool>,
    t: f64,
    last_loud: HashMap<usize, f64>, // rule index -> time its sources were last above threshold
}

impl Ducker {
    /// Call when the rule list changes: indices no longer match.
    pub fn rules_changed(&mut self) {
        self.last_loud.clear();
    }

    /// `peaks`: app name -> current peak (0..1) for every app with audio. Returns app -> gain (0..1).
    pub fn step(&mut self, rules: &[Rule], s: &Settings, peaks: &HashMap<String, f32>, dt_ms: f64) -> &HashMap<String, f32> {
        self.t += dt_ms;
        for (i, r) in rules.iter().enumerate() {
            if r.enabled && r.sources.iter().any(|src| peaks.get(src).copied().unwrap_or(0.0) > s.threshold) {
                self.last_loud.insert(i, self.t);
            }
        }

        let mut desired: HashMap<&str, f32> = peaks.keys().map(|a| (a.as_str(), 1.0)).collect();
        self.active = rules.iter().enumerate().map(|(i, r)| r.enabled && self.last_loud.get(&i).is_some_and(|&t| self.t - t <= s.hold_ms as f64)).collect();
        for (i, r) in rules.iter().enumerate() {
            if !self.active[i] {
                continue;
            }
            for (app, want) in desired.iter_mut() {
                let listed = r.targets.iter().any(|x| x == app);
                let hit = if r.all_others { !listed && !r.sources.iter().any(|x| x == app) } else { listed };
                if hit {
                    *want = want.min(r.level);
                }
            }
        }

        let mut gains = HashMap::with_capacity(desired.len());
        for (app, want) in desired {
            let g = self.gains.get(app).copied().unwrap_or(1.0);
            let ramp_ms = if want < g { s.attack_ms } else { s.release_ms } as f64;
            let step = if ramp_ms <= 0.0 { 1.0 } else { (dt_ms / ramp_ms) as f32 }; // linear: full 0..1 swing takes ramp_ms
            let next = if want < g { (g - step).max(want) } else { (g + step).min(want) };
            gains.insert(app.to_string(), next);
        }
        self.gains = gains;
        &self.gains
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peaks(p: &[(&str, f32)]) -> HashMap<String, f32> {
        p.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    fn run(d: &mut Ducker, rules: &[Rule], s: &Settings, p: &HashMap<String, f32>, ms: u32) -> HashMap<String, f32> {
        for _ in 0..ms / 10 {
            d.step(rules, s, p, 10.0);
        }
        d.gains.clone()
    }

    #[test]
    fn duck_hold_release() {
        let rules = [Rule { sources: vec!["discord.exe".into()], targets: vec!["spotify.exe".into()], ..Default::default() }];
        let s = Settings { threshold: 0.02, attack_ms: 100, hold_ms: 300, release_ms: 200 };
        let loud = peaks(&[("discord.exe", 0.5), ("spotify.exe", 0.3)]);
        let quiet = peaks(&[("discord.exe", 0.0), ("spotify.exe", 0.3)]);
        let mut d = Ducker::default();
        assert_eq!(run(&mut d, &rules, &s, &quiet, 100)["spotify.exe"], 1.0);
        let g = run(&mut d, &rules, &s, &loud, 50)["spotify.exe"];
        assert!(g > 0.2 && g < 1.0, "mid-attack {g}");
        assert_eq!(run(&mut d, &rules, &s, &loud, 200)["spotify.exe"], 0.2);
        assert_eq!(run(&mut d, &rules, &s, &quiet, 250)["spotify.exe"], 0.2); // still holding
        assert_eq!(run(&mut d, &rules, &s, &quiet, 400)["spotify.exe"], 1.0); // released
        assert_eq!(d.gains["discord.exe"], 1.0);
        assert_eq!(d.active, [false]);
    }

    #[test]
    fn all_others_and_lowest_level_wins() {
        let rules = [
            Rule { sources: vec!["discord.exe".into()], targets: vec!["foreground.exe".into()], all_others: true, level: 0.5, ..Default::default() },
            Rule { sources: vec!["discord.exe".into()], targets: vec!["spotify.exe".into()], level: 0.1, ..Default::default() },
        ];
        let s = Settings { attack_ms: 0, ..Default::default() };
        let mut d = Ducker::default();
        let g = d.step(&rules, &s, &peaks(&[("discord.exe", 1.0), ("spotify.exe", 0.1), ("firefox.exe", 0.1), ("foreground.exe", 0.0)]), 10.0);
        assert_eq!((g["discord.exe"], g["spotify.exe"], g["firefox.exe"], g["foreground.exe"]), (1.0, 0.1, 0.5, 1.0));
    }

    #[test]
    fn disabled_rule_and_legacy_format() {
        let mut r: Rule = serde_json::from_str(r#"{"sources":["discord.exe"],"targets":["spotify.exe"],"duck_all":false,"duck_amount":0.01}"#).unwrap();
        assert_eq!(r.level, 0.01);
        r.enabled = false;
        let s = Settings { attack_ms: 0, ..Default::default() };
        let g = Ducker::default().step(&[r], &s, &peaks(&[("discord.exe", 1.0), ("spotify.exe", 1.0)]), 10.0)["spotify.exe"];
        assert_eq!(g, 1.0);
    }
}
