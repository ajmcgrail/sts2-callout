import { listen } from "@tauri-apps/api/event";
import { imageUrl, type Card, type Monster, type MonsterDamage, type MonsterMove, type Relic, type ShowPayload } from "./types";
import { escapeHtml as esc, renderMarkup, titleCase } from "./markup";

const stack = document.getElementById("stack")!;
const timers = new Map<HTMLElement, number>();

function el<K extends keyof HTMLElementTagNameMap>(tag: K, cls?: string, html?: string): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (html !== undefined) e.innerHTML = html;
  return e;
}

function costLabel(c: Card, upgraded: boolean): string {
  if (c.is_x_cost) return "X";
  if (c.cost === null || c.cost < 0) return "";
  let cost = c.cost;
  const up = c.upgrade?.cost;
  if (upgraded && up !== undefined && up !== null) {
    const s = String(up);
    cost = /^[+-]/.test(s) ? cost + Number(s) : Number(s);
  }
  return String(Math.max(0, cost));
}

/** Hand-built card, used when the game render isn't available. */
function builtCard(c: Card, upgraded: boolean): HTMLElement {
  const card = el("div", `built color-${c.color.toLowerCase()} type-${c.type.toLowerCase()}`);
  const cost = costLabel(c, upgraded);
  if (cost) card.append(el("div", "cost", cost));
  card.append(el("div", "title" + (upgraded ? " upg" : ""), `${esc(c.name)}${upgraded ? "+" : ""}`));
  const art = el("img", "art");
  art.src = imageUrl("card_art", c.id);
  art.onerror = () => art.remove();
  card.append(art);
  card.append(el("div", "typeline", esc(`${c.rarity} ${c.type}`)));
  const text = upgraded ? c.upgrade_description ?? c.description : c.description;
  card.append(el("div", "text", renderMarkup(text)));
  return card;
}

function cardFace(c: Card, upgraded: boolean): HTMLElement {
  const fig = el("figure");
  const img = el("img", "render");
  img.alt = `${c.name}${upgraded ? "+" : ""}`;
  img.src = imageUrl(upgraded ? "card_upg" : "card", c.id);
  img.onerror = () => img.replaceWith(builtCard(c, upgraded));
  fig.append(img, el("figcaption", upgraded ? "upg" : "", upgraded ? "Upgraded" : "Base"));
  return fig;
}

function hasUpgrade(c: Card): boolean {
  return !!(c.image_url_card_upg || (c.upgrade && Object.keys(c.upgrade).length) ||
    (c.upgrade_description && c.upgrade_description !== c.description));
}

function cardPopup(c: Card, heard: string, beta: boolean): HTMLElement {
  const box = el("div", `popup card-popup color-${c.color.toLowerCase()}`);
  const head = el("div", "head");
  head.append(el("span", "name", esc(c.name)), el("span", "meta", esc(`${titleCase(c.color)} · ${c.rarity} ${c.type}`)));
  if (beta) head.append(el("span", "badge", "BETA"));
  if (heard) head.append(el("span", "heard", `“${esc(heard)}”`));
  box.append(head);

  const upg = hasUpgrade(c);
  const pair = el("div", "pair");
  pair.append(cardFace(c, false));
  if (upg) pair.append(cardFace(c, true));
  box.append(pair);

  const texts = el("div", "texts");
  texts.append(el("div", "t", renderMarkup(c.description)));
  if (upg) texts.append(el("div", "t upg", renderMarkup(c.upgrade_description ?? c.description)));
  else texts.append(el("div", "t none", "No upgrade"));
  box.append(texts);
  if (c.keywords?.length) box.append(el("div", "keywords", c.keywords.join(" · ")));
  return box;
}

function relicPopup(r: Relic, heard: string, beta: boolean): HTMLElement {
  const box = el("div", "popup relic-popup");
  const icon = el("img", "relic-icon");
  icon.src = imageUrl("relic", r.id);
  icon.alt = r.name;
  icon.onerror = () => icon.replaceWith(el("div", "relic-icon placeholder", "?"));
  const body = el("div", "relic-body");
  const head = el("div", "head");
  head.append(el("span", "name", esc(r.name)), el("span", "meta", esc(`${r.rarity} · ${titleCase(r.pool)}`)));
  if (beta) head.append(el("span", "badge", "BETA"));
  if (heard) head.append(el("span", "heard", `“${esc(heard)}”`));
  body.append(head, el("div", "desc", renderMarkup(r.description)));
  if (r.flavor && !/revealed in the future/i.test(r.flavor)) body.append(el("div", "flavor", renderMarkup(r.flavor)));
  box.append(icon, body);
  return box;
}

// ---- enemies ----

/** "PLATED_ARMOR" -> "Plated Armor" */
const powerName = (id: string) => esc(titleCase(id.toLowerCase().replace(/_/g, " ")));

function range(min?: number | null, max?: number | null): string {
  if (min == null) return "?";
  return max != null && max !== min ? `${min}–${max}` : `${min}`;
}

/** "11", "11×2", with the Ascension value when it differs: "11×2 (A 12×2)". */
function damageText(d: MonsterDamage): string {
  const fmt = (n?: number | null, hits?: number | null) => `${n ?? "?"}${hits && hits > 1 ? `×${hits}` : ""}`;
  const base = fmt(d.normal, d.hit_count);
  const asc = fmt(d.ascension ?? d.normal, d.hit_count_ascension ?? d.hit_count);
  return asc !== base ? `${base} <span class="asc">(A ${asc})</span>` : base;
}

function moveEffects(m: MonsterMove): string {
  const parts: string[] = [];
  if (m.damage && m.damage.normal != null) parts.push(`<span class="dmg">${damageText(m.damage)} dmg</span>`);
  if (m.block) parts.push(`<span class="blk">${m.block} block</span>`);
  if (m.heal) parts.push(`<span class="heal">heal ${m.heal}</span>`);
  for (const p of m.powers ?? []) {
    const who = p.target === "player" ? " on you" : p.target === "self" || !p.target ? "" : ` on ${esc(p.target)}`;
    parts.push(`<span class="pow">${powerName(p.power_id)}${p.amount != null ? ` ${p.amount}` : ""}${who}</span>`);
  }
  return parts.join(" · ");
}

/** The readable pattern Spire Codex provides, or one built from the state machine. */
function patternText(mon: Monster): string {
  const pat = mon.attack_pattern;
  if (pat?.description) return esc(pat.description);
  const name = (id?: string | null) => esc(mon.moves.find((m) => m.id === id)?.name ?? (id ? titleCase(id.toLowerCase().replace(/_/g, " ")) : "?"));
  const parts: string[] = [];
  const first = pat?.states?.find((st) => st.id === pat.initial_move);
  if (first?.type === "move") parts.push(`Starts with ${name(first.move_id)}`);
  else if (pat?.initial_move && mon.moves.some((m) => m.id === pat.initial_move)) parts.push(`Starts with ${name(pat.initial_move)}`);
  for (const st of pat?.states ?? []) {
    if (st.type === "random" && st.branches?.length) {
      const opts = st.branches.map((b) => `${name(b.move_id)}${b.repeat === "CannotRepeat" ? " (no repeat)" : ""}`);
      parts.push(`then random: ${opts.join(", ")}`);
    }
  }
  if (!parts.length && mon.moves.length) parts.push(`${pat?.type === "random" ? "Random" : "Moves"}: ${mon.moves.map((m) => m.name).join(", ")}`);
  return parts.join(", ");
}

function monsterPopup(mon: Monster, heard: string, beta: boolean): HTMLElement {
  const kind = (mon.type || "Normal").toLowerCase();
  const box = el("div", `popup monster-popup type-${kind}`);
  const acts = [...new Set((mon.encounters ?? []).map((e) => e.act).filter(Boolean))].join(", ");
  const head = el("div", "head");
  head.append(el("span", "name", esc(mon.name)), el("span", "meta", esc([kind === "normal" ? "Enemy" : mon.type, acts].filter(Boolean).join(" · "))));
  if (beta) head.append(el("span", "badge", "BETA"));
  if (heard) head.append(el("span", "heard", `“${esc(heard)}”`));
  box.append(head);

  const top = el("div", "monster-top");
  const img = el("img", "monster-img");
  img.src = imageUrl("monster", mon.id);
  img.alt = mon.name;
  img.onerror = () => img.remove();
  const stats = el("div", "monster-stats");
  const hp = range(mon.min_hp, mon.max_hp);
  const hpAsc = range(mon.min_hp_ascension, mon.max_hp_ascension);
  stats.append(el("div", "hp", `<span class="label">HP</span> ${hp}${hpAsc !== "?" && hpAsc !== hp ? ` <span class="asc">(A ${hpAsc})</span>` : ""}`));
  const innate = (mon.innate_powers ?? []).map((p) => `${powerName(p.power_id)}${p.amount != null ? ` ${p.amount}` : ""}`);
  if (innate.length) stats.append(el("div", "row", `<span class="label">Starts with</span> <span class="pow">${innate.join(", ")}</span>`));
  const pattern = patternText(mon);
  if (pattern) stats.append(el("div", "row", `<span class="label">Pattern</span> ${pattern}`));
  top.append(img, stats);
  box.append(top);

  const moves = el("div", "moves");
  for (const m of mon.moves) {
    const intents = m.intent.split("+").map((i) => i.trim()).filter(Boolean)
      .map((i) => `<span class="intent i-${i.toLowerCase().replace(/[^a-z]/g, "")}">${esc(i)}</span>`).join("");
    const row = el("div", "move");
    row.append(el("span", "move-name", esc(m.name)), el("span", "intents", intents), el("span", "effects", moveEffects(m)));
    moves.append(row);
  }
  box.append(moves);
  return box;
}

function dismiss(p: HTMLElement) {
  window.clearTimeout(timers.get(p));
  timers.delete(p);
  p.classList.remove("pulse");
  p.classList.add("leaving");
  p.addEventListener("animationend", (e) => e.animationName === "leave" && p.remove());
  window.setTimeout(() => p.remove(), 800);
}

function arm(p: HTMLElement, secs: number) {
  window.clearTimeout(timers.get(p));
  timers.set(p, window.setTimeout(() => dismiss(p), Math.max(1, secs) * 1000));
}

listen<ShowPayload>("show-item", ({ payload: s }) => {
  document.documentElement.style.setProperty("--scale", String(s.scale || 1));
  const key = `${s.kind}:${s.id}`;
  const existing = stack.querySelector<HTMLElement>(`[data-key="${CSS.escape(key)}"]:not(.leaving)`);
  if (existing) {
    // Already showing: move to top and restart its timer.
    stack.prepend(existing);
    existing.classList.remove("pulse");
    void existing.offsetWidth;
    existing.classList.add("pulse");
    arm(existing, s.display_secs);
    return;
  }
  const p = s.card ? cardPopup(s.card, s.heard, s.beta_only)
    : s.relic ? relicPopup(s.relic, s.heard, s.beta_only)
    : s.monster ? monsterPopup(s.monster, s.heard, s.beta_only)
    : null;
  if (!p) return;
  p.dataset.key = key;
  stack.prepend(p);
  arm(p, s.display_secs);
  const live = () => [...stack.children].filter((c) => !c.classList.contains("leaving")) as HTMLElement[];
  live().slice(Math.max(1, s.max_onscreen)).forEach(dismiss);
  // Don't run off the bottom of the screen: drop the oldest until it fits.
  requestAnimationFrame(() => {
    let items = live();
    const zoom = s.scale || 1;
    while (items.length > 1 && items.reduce((h, e) => h + e.offsetHeight + 14, 20) * zoom > window.innerHeight) {
      dismiss(items[items.length - 1]);
      items = live();
    }
  });
});

listen("clear-overlay", () => [...stack.children].forEach((c) => dismiss(c as HTMLElement)));
