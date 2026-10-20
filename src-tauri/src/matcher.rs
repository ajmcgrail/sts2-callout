//! Finds card / relic names inside a speech-recognition transcript.
//!
//! Exact phrase matches are tried first (longest names win, so "Perfected
//! Strike" beats "Strike"), then a fuzzy pass catches recognizer misspellings
//! of longer names ("neutralise" -> Neutralize, "body slam" -> Bodyslam).

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::data::GameData;
use crate::settings::Settings;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Card,
    Relic,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct ItemRef {
    pub kind: Kind,
    pub id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Match {
    pub item: ItemRef,
    /// The words of the transcript that matched.
    pub heard: String,
    pub score: f64,
}

struct Entry {
    compact: String,
    sound: String,
    n_tokens: usize,
    items: Vec<ItemRef>,
}

pub struct Matcher {
    entries: Vec<Entry>,
    exact: HashMap<String, usize>,
    max_tokens: usize,
    fuzzy: bool,
}

const FUZZY_MIN_CHARS: usize = 7;
/// Shortest sound-alike key that may match; shorter keys collide too easily.
const SOUND_MIN_LEN: usize = 4;

/// Rough English sound-alike key (Metaphone-like): vowels after the first
/// letter dropped, voiced/unvoiced pairs merged (b/p, d/t, g/k, v/f, z/s),
/// common digraphs folded, repeats collapsed. Recognizers garble names in
/// crosstalk mostly in ways that keep this key.
pub fn phonetic(word: &str) -> String {
    let w: Vec<char> = word.chars().filter(|c| c.is_ascii_alphabetic()).map(|c| c.to_ascii_lowercase()).collect();
    let vowel = |c: char| "aeiouy".contains(c);
    let mut out = String::new();
    let mut i = 0;
    while i < w.len() {
        let c = w[i];
        let next = w.get(i + 1).copied();
        let mut skip_next = false;
        let code = match c {
            _ if vowel(c) => (i == 0).then_some('a'),
            'b' | 'p' => {
                skip_next = c == 'p' && next == Some('h');
                Some(if skip_next { 'f' } else { 'p' })
            }
            'c' => match next {
                Some('h') => {
                    skip_next = true;
                    Some('x')
                }
                Some('e' | 'i' | 'y') => Some('s'),
                _ => Some('k'),
            },
            'd' | 't' => {
                skip_next = c == 't' && next == Some('h');
                Some(if skip_next { '0' } else { 't' })
            }
            'f' | 'v' => Some('f'),
            'g' => match next {
                Some('h') => {
                    skip_next = true;
                    (i == 0).then_some('k')
                }
                Some('e' | 'i' | 'y') => Some('j'),
                _ => Some('k'),
            },
            'h' => (next.is_some_and(vowel) && (i == 0 || !vowel(w[i - 1]))).then_some('h'),
            'k' | 'q' => Some('k'),
            's' => {
                skip_next = next == Some('h');
                Some(if skip_next { 'x' } else { 's' })
            }
            'w' => (next.is_some_and(vowel) && (i == 0 || !vowel(w[i - 1]))).then_some('w'),
            'x' => {
                if !out.ends_with('k') {
                    out.push('k');
                }
                Some('s')
            }
            'z' => Some('s'),
            'j' | 'l' | 'm' | 'n' | 'r' => Some(c),
            _ => None,
        };
        if let Some(k) = code {
            if !out.ends_with(k) {
                out.push(k);
            }
        }
        i += if skip_next { 2 } else { 1 };
    }
    out
}

/// Is window `[i, i+k)` taken? With `may_cover`, shorter earlier matches lying
/// entirely inside the window don't count.
fn blocked(used: &[Option<(usize, usize)>], i: usize, k: usize, may_cover: bool) -> bool {
    used[i..i + k].iter().flatten().any(|&(s, l)| !(may_cover && s >= i && s + l <= i + k && l < k))
}

pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\'' | '’' | '‘' | '`' => {}
            '&' => out.push_str(" and "),
            '+' => out.push_str(" plus "),
            c if c.is_alphanumeric() => out.extend(c.to_lowercase()),
            _ => out.push(' '),
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

impl Matcher {
    pub fn new(data: &GameData, settings: &Settings) -> Self {
        let ignored: Vec<String> = settings.ignored_names.iter().map(|n| normalize(n)).collect();
        let pref = settings.preferred_character.to_lowercase();

        // Sort key among same-named items: (is "???" variant, not preferred character, compendium order)
        type Rank = (bool, bool, i64);
        // normalized name -> candidates
        let mut groups: HashMap<String, Vec<(Kind, Rank, String)>> = HashMap::new();
        let mut add = |name: &str, kind: Kind, owner: &str, order: i64, id: &str| {
            let key = normalize(name);
            if key.is_empty() || ignored.contains(&key) {
                return;
            }
            if settings.min_single_word_len > 0
                && !key.contains(' ')
                && key.chars().count() < settings.min_single_word_len
            {
                return;
            }
            // Prefer real entries over "???" variants, then the chosen character.
            let rank = (name.contains('?'), !pref.is_empty() && owner != pref, order);
            groups.entry(key).or_default().push((kind, rank, id.to_string()));
        };
        if settings.show_cards {
            for c in &data.cards {
                add(&c.name, Kind::Card, &c.color.to_lowercase(), c.compendium_order, &c.id);
            }
        }
        if settings.show_relics {
            for r in &data.relics {
                add(&r.name, Kind::Relic, &r.pool.to_lowercase(), r.compendium_order, &r.id);
            }
        }

        let mut entries = Vec::new();
        let mut exact = HashMap::new();
        let mut max_tokens = 1;
        for (key, mut cands) in groups {
            cands.sort_by_key(|c| c.1);
            let mut items: Vec<ItemRef> = Vec::new();
            for (kind, _, id) in cands {
                if !items.iter().any(|i| i.kind == kind) {
                    items.push(ItemRef { kind, id });
                }
            }
            let n_tokens = key.split(' ').count();
            max_tokens = max_tokens.max(n_tokens);
            exact.insert(key.clone(), entries.len());
            let compact = key.replace(' ', "");
            entries.push(Entry { sound: phonetic(&compact), compact, n_tokens, items });
        }
        Matcher { entries, exact, max_tokens, fuzzy: settings.fuzzy }
    }

    pub fn find(&self, text: &str) -> Vec<Match> {
        let norm = normalize(text);
        let tokens: Vec<&str> = norm.split(' ').filter(|t| !t.is_empty()).collect();
        // token -> (start, len) of the match that claimed it
        let mut used: Vec<Option<(usize, usize)>> = vec![None; tokens.len()];
        let mut hits: Vec<(usize, Match)> = Vec::new();

        // Exact pass, longest phrases first.
        for n in (1..=self.max_tokens.min(tokens.len())).rev() {
            for i in 0..=tokens.len() - n {
                if used[i..i + n].iter().any(|u| u.is_some()) {
                    continue;
                }
                let window = tokens[i..i + n].join(" ");
                let idx = self.exact.get(&window).or_else(|| {
                    // plurals / possessives: "two strikes", "bashes"
                    let w = window.strip_suffix("es").filter(|w| self.exact.contains_key(*w));
                    let w = w.or_else(|| window.strip_suffix('s'));
                    w.and_then(|w| self.exact.get(w))
                });
                if let Some(&idx) = idx {
                    used[i..i + n].iter_mut().for_each(|u| *u = Some((i, n)));
                    for item in &self.entries[idx].items {
                        hits.push((i, Match { item: item.clone(), heard: window.clone(), score: 1.0 }));
                    }
                }
            }
        }

        if self.fuzzy {
            // sound-alike keys of every window, shared by all entries
            let mut sounds: HashMap<(usize, usize), String> = HashMap::new();
            let mut cands: Vec<(f64, usize, usize, usize)> = Vec::new(); // score, start, len, entry
            for (ei, e) in self.entries.iter().enumerate() {
                if e.compact.len() < FUZZY_MIN_CHARS {
                    continue;
                }
                let threshold = if e.compact.len() >= 12 { 0.82 } else { 0.86 };
                for k in e.n_tokens.saturating_sub(1).max(1)..=e.n_tokens + 1 {
                    if k > tokens.len() {
                        break;
                    }
                    for i in 0..=tokens.len() - k {
                        if blocked(&used, i, k, k > 1) {
                            continue;
                        }
                        let window: String = tokens[i..i + k].concat();
                        // cheap length filter before edit distance
                        let (a, b) = (window.len() as f64, e.compact.len() as f64);
                        if (a - b).abs() / b > 0.25 {
                            continue;
                        }
                        let s = strsim::normalized_levenshtein(&window, &e.compact);
                        if s >= threshold {
                            cands.push((s, i, k, ei));
                        } else if blocked(&used, i, k, false) {
                            continue; // only a confident spelling match may override an exact one
                        } else if s >= 0.5 && e.sound.len() >= SOUND_MIN_LEN {
                            // Misheard but sounds the same: "a cobeco" -> Akabeko
                            let ws = sounds.entry((i, k)).or_insert_with(|| phonetic(&window));
                            let ss = strsim::normalized_levenshtein(ws, &e.sound);
                            if ss >= 0.85 {
                                cands.push((0.6 * ss + 0.2 * s, i, k, ei));
                            }
                        }
                    }
                }
            }
            cands.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.2.cmp(&a.2)));
            for (score, i, k, ei) in cands {
                if blocked(&used, i, k, score >= 0.82) {
                    continue;
                }
                // "perfect it strike": the longer fuzzy name replaces the exact "strike" inside it
                hits.retain(|(pos, _)| *pos < i || *pos >= i + k);
                used[i..i + k].iter_mut().for_each(|u| *u = Some((i, k)));
                let heard = tokens[i..i + k].join(" ");
                for item in &self.entries[ei].items {
                    hits.push((i, Match { item: item.clone(), heard: heard.clone(), score }));
                }
            }
        }

        hits.sort_by_key(|(pos, _)| *pos);
        hits.into_iter().map(|(_, m)| m).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::data::{Card, GameData, Meta, Relic};

    fn card(id: &str, name: &str, color: &str, order: i64) -> Card {
        Card { id: id.into(), name: name.into(), color: color.into(), compendium_order: order, ..Default::default() }
    }
    fn relic(id: &str, name: &str) -> Relic {
        Relic { id: id.into(), name: name.into(), pool: "shared".into(), ..Default::default() }
    }
    fn data() -> GameData {
        GameData {
            cards: vec![
                card("STRIKE_IRONCLAD", "Strike", "ironclad", 1),
                card("STRIKE_SILENT", "Strike", "silent", 2),
                card("PERFECTED_STRIKE", "Perfected Strike", "ironclad", 3),
                card("BASH", "Bash", "ironclad", 4),
                card("NEUTRALIZE", "Neutralize", "silent", 5),
                card("BODYSLAM", "Body Slam", "ironclad", 6),
                card("SIC_EM", "Sic 'Em", "necrobinder", 7),
            ],
            relics: vec![
                relic("ANCHOR", "Anchor"),
                relic("ANCHOR_Q", "Anchor???"),
                relic("PANDORAS_BOX", "Pandora's Box"),
            ],
            meta: Meta::default(),
        }
    }
    fn ids(ms: &[Match]) -> Vec<&str> {
        ms.iter().map(|m| m.item.id.as_str()).collect()
    }

    #[test]
    fn longest_match_wins_and_order_kept() {
        let m = Matcher::new(&data(), &Settings::default());
        let r = m.find("I'd take Perfected Strike, then Bash. Maybe a strike.");
        assert_eq!(ids(&r), ["PERFECTED_STRIKE", "BASH", "STRIKE_IRONCLAD"]);
    }

    #[test]
    fn punctuation_plurals_and_variants() {
        let m = Matcher::new(&data(), &Settings::default());
        assert_eq!(ids(&m.find("Pandoras box!! and an anchor")), ["PANDORAS_BOX", "ANCHOR"]);
        assert_eq!(ids(&m.find("two strikes")), ["STRIKE_IRONCLAD"]);
        assert_eq!(ids(&m.find("sic em boys")), ["SIC_EM"]);
    }

    #[test]
    fn fuzzy_and_compound() {
        let m = Matcher::new(&data(), &Settings::default());
        assert_eq!(ids(&m.find("I'll neutralise that")), ["NEUTRALIZE"]);
        assert_eq!(ids(&m.find("bodyslam time")), ["BODYSLAM"]);
        assert!(m.find("nothing to see here").is_empty());
    }

    #[test]
    fn sound_alikes() {
        assert_eq!(phonetic("akabeko"), phonetic("acobeco"));
        assert_eq!(phonetic("phantasmal"), phonetic("fantazmal"));
        let mut d = data();
        d.relics.push(relic("AKABEKO", "Akabeko"));
        let m = Matcher::new(&d, &Settings::default());
        assert_eq!(ids(&m.find("I'd love an acobeco right now")), ["AKABEKO"]);
        assert_eq!(ids(&m.find("a cobeco")), ["AKABEKO"]);
        assert_eq!(ids(&m.find("perfect it strike")), ["PERFECTED_STRIKE"]);
        assert_eq!(phonetic("flower"), phonetic("flour"));
    }

    #[test]
    fn settings_respected() {
        let s = Settings {
            preferred_character: "silent".into(),
            ignored_names: vec!["bash".into()],
            ..Settings::default()
        };
        let m = Matcher::new(&data(), &s);
        assert_eq!(ids(&m.find("bash then strike")), ["STRIKE_SILENT"]);
    }
}

#[cfg(test)]
mod real_data {
    //! `STS2_DATA=<dir with cards.json + relics.json> cargo test real_data -- --nocapture`
    use super::*;

    #[test]
    fn transcript_samples() {
        let Ok(dir) = std::env::var("STS2_DATA") else { return };
        let dir = std::path::Path::new(&dir);
        let data = GameData {
            cards: serde_json::from_slice(&std::fs::read(dir.join("cards.json")).unwrap()).unwrap(),
            relics: serde_json::from_slice(&std::fs::read(dir.join("relics.json")).unwrap()).unwrap(),
            meta: Default::default(),
        };
        let m = Matcher::new(&data, &Settings::default());
        if let Ok(corpus) = std::env::var("STS2_CORPUS") {
            // ordinary chatter: every hit printed here is a false positive candidate
            for line in std::fs::read_to_string(corpus).unwrap().lines() {
                for x in m.find(line) {
                    println!("FP? {:?} -> {} ({:.2}) in: {line}", x.heard, x.item.id, x.score);
                }
            }
        }
        for line in [
            "Okay so I'm going to take the Akabeko here, and I think Bash plus is fine.",
            "Oh no, I hope we see a Pandora's box or maybe neutralise and footwork.",
            "Alright chat, what do we think about this path, the elite looks scary.",
            "I'd rather have Body Slam than another defend, honestly.",
            "Thanks for the sub! Let's go to the shop and buy the anchor.",
        ] {
            let found: Vec<String> = m.find(line).iter().map(|x| format!("{}({:.2})", x.item.id, x.score)).collect();
            println!("{line}\n   -> {found:?}");
        }
    }
}
