export type Kind = "card" | "relic";

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

export interface ShowPayload {
  kind: Kind;
  id: string;
  heard: string;
  card: Card | null;
  relic: Relic | null;
  display_secs: number;
  max_onscreen: number;
  scale: number;
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
}

export interface Status {
  data_loaded: boolean;
  cards: number;
  relics: number;
  fetched_at: number;
  source: string;
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
export function imageUrl(kind: "card" | "card_upg" | "card_art" | "relic", id: string): string {
  const base = navigator.userAgent.includes("Windows") ? "http://stsimg.localhost" : "stsimg://localhost";
  return `${base}/${kind}/${encodeURIComponent(id)}`;
}
