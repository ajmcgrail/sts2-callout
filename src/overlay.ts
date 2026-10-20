import { listen } from "@tauri-apps/api/event";
import { imageUrl, type Card, type Relic, type ShowPayload } from "./types";
import { renderMarkup, titleCase } from "./markup";

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
  card.append(el("div", "title" + (upgraded ? " upg" : ""), `${c.name}${upgraded ? "+" : ""}`));
  const art = el("img", "art");
  art.src = imageUrl("card_art", c.id);
  art.onerror = () => art.remove();
  card.append(art);
  card.append(el("div", "typeline", `${c.rarity} ${c.type}`));
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

function cardPopup(c: Card, heard: string): HTMLElement {
  const box = el("div", `popup card-popup color-${c.color.toLowerCase()}`);
  const head = el("div", "head");
  head.append(el("span", "name", c.name), el("span", "meta", `${titleCase(c.color)} · ${c.rarity} ${c.type}`));
  if (heard) head.append(el("span", "heard", `“${heard}”`));
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

function relicPopup(r: Relic, heard: string): HTMLElement {
  const box = el("div", "popup relic-popup");
  const icon = el("img", "relic-icon");
  icon.src = imageUrl("relic", r.id);
  icon.alt = r.name;
  icon.onerror = () => icon.replaceWith(el("div", "relic-icon placeholder", "?"));
  const body = el("div", "relic-body");
  const head = el("div", "head");
  head.append(el("span", "name", r.name), el("span", "meta", `${r.rarity} · ${titleCase(r.pool)}`));
  if (heard) head.append(el("span", "heard", `“${heard}”`));
  body.append(head, el("div", "desc", renderMarkup(r.description)));
  if (r.flavor && !/revealed in the future/i.test(r.flavor)) body.append(el("div", "flavor", renderMarkup(r.flavor)));
  box.append(icon, body);
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
  const p = s.card ? cardPopup(s.card, s.heard) : s.relic ? relicPopup(s.relic, s.heard) : null;
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
