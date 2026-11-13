export type Kind = "card" | "relic" | "monster";

export interface Card {
  id: string;
  name: string;
  description: string;
  upgrade_description: string | null;
  cost: number | null;
  is_x_cost: boolean | null;
  star_cost: number | null;
  is_x_star_cost: boolean | null;
  type: string;
  rarity: string;
  color: string;
  keywords: string[] | null;
  upgrade: Record<string, string | number | null> | null;
  image_url_card: string | null;
  image_url_card_upg: string | null;
}

export interface Relic {
  id: string;
  name: string;
  description: string;
  flavor: string | null;
  rarity: string;
  pool: string;
}

export interface MonsterDamage {
  normal?: number | null;
  ascension?: number | null;
  hit_count?: number | null;
  hit_count_ascension?: number | null;
}

export interface MonsterPower {
  power_id: string;
  amount?: number | null;
  target?: string | null;
}

export interface MonsterMove {
  id: string;
  name: string;
  intent: string;
  damage?: MonsterDamage | null;
  block?: number | null;
  heal?: number | null;
  powers?: MonsterPower[] | null;
}

export interface PatternState {
  id: string;
  type: "move" | "random" | "conditional" | string;
  move_id?: string;
  next?: string;
  branches?: { move_id: string; weight?: number; repeat?: string; condition?: string }[];
}

export interface Monster {
  id: string;
  name: string;
  type: string;
  min_hp?: number | null;
  max_hp?: number | null;
  min_hp_ascension?: number | null;
  max_hp_ascension?: number | null;
  moves: MonsterMove[];
  innate_powers?: { power_id: string; amount?: number | null }[] | null;
  encounters?: { encounter_name?: string; room_type?: string; act?: string | null }[] | null;
  attack_pattern?: { type?: string; initial_move?: string | null; states?: PatternState[]; description?: string } | null;
}

export interface ShowPayload {
  kind: Kind;
  id: string;
  heard: string;
  card: Card | null;
  relic: Relic | null;
  monster: Monster | null;
  display_secs: number;
  max_onscreen: number;
  scale: number;
  /** Only in the beta branch so far. */
  beta_only: boolean;
}

export interface Settings {
  device: string | null;
  model: string;
  max_phrase_secs: number;
  use_vad: boolean;
  accuracy: "fast" | "balanced" | "best";
  silence_threshold: number;
  display_secs: number;
  cooldown_secs: number;
  max_onscreen: number;
  overlay_side: "left" | "right";
  overlay_scale: number;
  preferred_character: string;
  ignored_names: string[];
  min_single_word_len: number;
  fuzzy: boolean;
  show_cards: boolean;
  show_relics: boolean;
  show_monsters: boolean;
  data_channel: "stable" | "beta";
}

export interface Status {
  data_loaded: boolean;
  cards: number;
  relics: number;
  fetched_at: number;
  source: string;
  channel: "stable" | "beta" | null;
  version: string | null;
  listen: ListenState;
  cache_dir: string;
  model_present: boolean;
  models: { name: string; label: string }[];
}

export interface ListenState {
  phase: "idle" | "starting" | "listening";
  label: string | null;
  error: string | null;
}

export interface DeviceInfo {
  key: string;
  label: string;
  host: string;
  loopback: boolean;
}

export interface Match {
  item: { kind: Kind; id: string };
  heard: string;
  score: number;
}

export interface SearchHit {
  kind: Kind;
  id: string;
  name: string;
  detail: string;
}

/** URL of a locally cached image (served by the Rust `stsimg` protocol). */
export function imageUrl(kind: "card" | "card_upg" | "card_art" | "relic" | "monster", id: string): string {
  const base = navigator.userAgent.includes("Windows") ? "http://stsimg.localhost" : "stsimg://localhost";
  return `${base}/${kind}/${encodeURIComponent(id)}`;
}
