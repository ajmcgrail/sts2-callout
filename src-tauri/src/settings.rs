use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// Audio device key from `list_devices`; `None` picks system output automatically.
    pub device: Option<String>,
    /// Whisper model name, see `stt::MODELS`.
    pub model: String,
    /// Longest stretch of continuous speech sent to whisper at once.
    pub max_phrase_secs: f32,
    /// Split audio into phrases with a voice-activity model.
    pub use_vad: bool,
    /// "fast", "balanced" or "best"; see `stt::whisper_params`.
    pub accuracy: String,
    /// RMS level below which a chunk is treated as silence and skipped.
    pub silence_threshold: f32,
    /// How long a popup stays on screen.
    pub display_secs: f32,
    /// Don't re-show the same item within this many seconds.
    pub cooldown_secs: f32,
    pub max_onscreen: usize,
    pub overlay_side: String,
    pub overlay_scale: f32,
    /// For duplicate names (Strike, Defend): prefer this character's version.
    pub preferred_character: String,
    pub ignored_names: Vec<String>,
    /// Ignore one-word names shorter than this (0 = off).
    pub min_single_word_len: usize,
    pub fuzzy: bool,
    pub show_cards: bool,
    pub show_relics: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            device: None,
            model: "small.en-q5_1".into(),
            max_phrase_secs: 8.0,
            use_vad: true,
            accuracy: "balanced".into(),
            silence_threshold: 0.003,
            display_secs: 9.0,
            cooldown_secs: 20.0,
            max_onscreen: 3,
            overlay_side: "right".into(),
            overlay_scale: 1.0,
            preferred_character: String::new(),
            ignored_names: Vec::new(),
            min_single_word_len: 0,
            fuzzy: true,
            show_cards: true,
            show_relics: true,
        }
    }
}

impl Settings {
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|b| serde_json::from_slice::<Settings>(&b).ok())
            .unwrap_or_default()
            .sanitized()
    }

    pub fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let tmp = path.with_extension("json.part");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Clamp values into usable ranges (an emptied number field arrives as 0).
    pub fn sanitized(mut self) -> Self {
        let d = Settings::default();
        let fix = |v: f32, lo: f32, hi: f32, default: f32| if v.is_finite() && v >= lo { v.min(hi) } else { default };
        self.max_phrase_secs = fix(self.max_phrase_secs, 2.0, 20.0, d.max_phrase_secs);
        self.silence_threshold = fix(self.silence_threshold, 0.0, 0.2, d.silence_threshold);
        self.display_secs = fix(self.display_secs, 1.0, 120.0, d.display_secs);
        self.cooldown_secs = fix(self.cooldown_secs, 0.0, 3600.0, d.cooldown_secs);
        self.overlay_scale = fix(self.overlay_scale, 0.4, 2.0, d.overlay_scale);
        self.max_onscreen = self.max_onscreen.clamp(1, 8);
        self.min_single_word_len = self.min_single_word_len.min(20);
        if !["fast", "balanced", "best"].contains(&self.accuracy.as_str()) {
            self.accuracy = d.accuracy;
        }
        if !["left", "right"].contains(&self.overlay_side.as_str()) {
            self.overlay_side = d.overlay_side;
        }
        self.ignored_names.retain(|n| !n.trim().is_empty());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitized_repairs_bad_values() {
        let s = Settings {
            overlay_scale: 0.0,
            display_secs: f32::NAN,
            max_onscreen: 0,
            accuracy: "turbo".into(),
            ignored_names: vec!["".into(), " Bash ".into()],
            ..Settings::default()
        }
        .sanitized();
        assert_eq!(s.overlay_scale, 1.0);
        assert_eq!(s.display_secs, Settings::default().display_secs);
        assert_eq!(s.max_onscreen, 1);
        assert_eq!(s.accuracy, "balanced");
        assert_eq!(s.ignored_names, vec![" Bash ".to_string()]);
    }

    #[test]
    fn old_settings_files_still_load() {
        let dir = std::env::temp_dir().join(format!("sts2-settings-{}", std::process::id()));
        let path = dir.join("settings.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, r#"{"model":"base.en","chunk_secs":4.0,"fast_mode":true,"overlay_scale":0}"#).unwrap();
        let s = Settings::load(&path);
        assert_eq!(s.model, "base.en");
        assert_eq!(s.overlay_scale, 1.0);
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path).model, "base.en");
        std::fs::remove_dir_all(dir).unwrap();
    }
}
