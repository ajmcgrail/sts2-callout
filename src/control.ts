import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { DeviceInfo, ListenState, Match, SearchHit, Settings, ShowPayload, Status } from "./types";

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;

const deviceSel = $<HTMLSelectElement>("device");
const modelSel = $<HTMLSelectElement>("model");
const listenBtn = $<HTMLButtonElement>("listen");
const listenStatus = $("listen-status");
const log = $<HTMLOListElement>("log");
const form = $<HTMLFormElement>("settings");
const progress = $<HTMLProgressElement>("model-progress");

let settings: Settings;
let phase: ListenState["phase"] = "idle";

function addLog(html: string, cls = "") {
  const li = document.createElement("li");
  li.className = cls;
  li.innerHTML = `<time>${new Date().toLocaleTimeString()}</time> ${html}`;
  log.prepend(li);
  while (log.children.length > 200) log.lastElementChild!.remove();
}

const esc = (s: string) => s.replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]!);

function highlight(text: string, words: string[]): string {
  let html = esc(text);
  for (const w of words) {
    const pattern = w.split(" ").map((p) => p.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("[^a-z0-9]+");
    html = html.replace(new RegExp(`(${pattern})`, "i"), "<mark>$1</mark>");
  }
  return html;
}

async function refreshStatus() {
  const st = await invoke<Status>("get_status");
  const pill = $("data-status");
  if (st.data_loaded) {
    const when = st.fetched_at ? new Date(st.fetched_at * 1000).toLocaleDateString() : "unknown date";
    pill.textContent = `${st.cards} cards · ${st.relics} relics · cached ${when}`;
    pill.className = "pill ok";
  } else {
    pill.textContent = "No data yet";
    pill.className = "pill warn";
  }
  $("cache-dir").textContent = st.cache_dir;
  if (!modelSel.options.length) {
    for (const m of st.models) modelSel.add(new Option(m.label, m.name));
  }
  if (![...modelSel.options].some((o) => o.value === settings.model)) modelSel.add(new Option(settings.model, settings.model));
  modelSel.value = settings.model;
  renderListen(st.listen);
}

/** Draw the listen controls from the backend's state (the only source of truth). */
function renderListen(s: ListenState) {
  const was = phase;
  phase = s.phase;
  listenBtn.textContent = { idle: "Start listening", starting: "Cancel", listening: "Stop listening" }[s.phase];
  listenBtn.classList.toggle("danger", s.phase !== "idle");
  deviceSel.disabled = modelSel.disabled = s.phase !== "idle";
  if (s.phase === "starting") {
    if (was !== "starting") listenStatus.textContent = "Starting…";
  } else {
    listenStatus.textContent = s.phase === "listening" ? `Listening to ${s.label ?? "audio"}` : "";
    progress.hidden = true;
  }
  if (s.phase !== "listening") $("level").style.width = "0";
  if (s.phase === "listening" && was !== "listening") addLog(`Listening to <b>${esc(s.label ?? "audio")}</b>`, "sys");
  if (s.phase === "idle" && was === "listening" && !s.error) addLog("Stopped listening", "sys");
  if (s.error && s.error !== "cancelled") addLog(esc(s.error), "err");
}

async function loadDevices() {
  const devices = await invoke<DeviceInfo[]>("list_devices");
  const groups = new Map<string, HTMLOptGroupElement>();
  // System-output sources first, they're what this app is for.
  devices.sort((a, b) => Number(b.loopback) - Number(a.loopback));
  for (const d of devices) {
    const name = d.loopback ? "System output" : `Inputs (${d.host})`;
    let g = groups.get(name);
    if (!g) {
      g = document.createElement("optgroup");
      g.label = name;
      groups.set(name, g);
      deviceSel.append(g);
    }
    g.append(new Option(d.loopback ? d.label : `${d.label}`, d.key));
  }
  if (settings.device && ![...deviceSel.options].some((o) => o.value === settings.device)) {
    deviceSel.add(new Option(`(missing) ${settings.device.split("|").pop()}`, settings.device));
  }
  deviceSel.value = settings.device ?? "";
}

function fillForm(s: Settings) {
  for (const [k, v] of Object.entries(s)) {
    const input = form.elements.namedItem(k) as HTMLInputElement | HTMLSelectElement | HTMLTextAreaElement | null;
    if (!input) continue;
    if (input instanceof HTMLInputElement && input.type === "checkbox") input.checked = Boolean(v);
    else if (Array.isArray(v)) input.value = v.join("\n");
    else input.value = String(v ?? "");
  }
}

function readForm(): Settings {
  const s: Settings = { ...settings };
  const rec = s as unknown as Record<string, unknown>;
  for (const el of Array.from(form.elements)) {
    const input = el as HTMLInputElement;
    if (!input.name) continue;
    if (input.type === "checkbox") rec[input.name] = input.checked;
    else if (input.type === "number") rec[input.name] = Number(input.value);
    else if (input.name === "ignored_names") rec[input.name] = input.value.split("\n").map((x) => x.trim()).filter(Boolean);
    else rec[input.name] = input.value;
  }
  s.device = deviceSel.value || null;
  s.model = modelSel.value || settings.model;
  return s;
}

async function save(s: Settings) {
  // The backend clamps out-of-range values; show what was actually saved.
  settings = await invoke<Settings>("save_settings", { settings: s });
  fillForm(settings);
}

form.addEventListener("submit", async (e) => {
  e.preventDefault();
  try {
    await save(readForm());
    $("saved").textContent = "Saved";
    setTimeout(() => ($("saved").textContent = ""), 1500);
  } catch (err) {
    $("saved").textContent = String(err);
  }
});

deviceSel.addEventListener("change", () => save({ ...settings, device: deviceSel.value || null }));
modelSel.addEventListener("change", () => save({ ...settings, model: modelSel.value }));

// Never disabled: while starting it's "Cancel", while listening it's "Stop".
// The result arrives as a `listen-state` event, so the promise is only for errors.
listenBtn.addEventListener("click", () => {
  if (phase === "idle") {
    invoke<string>("start_listening").catch(() => {}); // failures are reported via listen-state
  } else {
    invoke("stop_listening").catch((err) => addLog(esc(String(err)), "err"));
  }
});

$("refresh").addEventListener("click", async () => {
  const btn = $<HTMLButtonElement>("refresh");
  btn.disabled = true;
  try {
    const n = await invoke<number>("refresh_data");
    addLog(`Downloaded ${n} cards and relics`, "sys");
  } catch (err) {
    addLog(esc(String(err)), "err");
  } finally {
    btn.disabled = false;
    refreshStatus();
  }
});

$("clear-log").addEventListener("click", () => (log.innerHTML = ""));

async function simulate() {
  const input = $<HTMLInputElement>("simulate");
  if (!input.value.trim()) return;
  const found = await invoke<Match[]>("test_text", { text: input.value });
  addLog(`<i>simulated:</i> ${highlight(input.value, found.map((m) => m.heard))}${found.length ? "" : ' <span class="muted">(no matches)</span>'}`);
}
$("simulate-go").addEventListener("click", simulate);
$("simulate").addEventListener("keydown", (e) => e.key === "Enter" && simulate());

let searchTimer = 0;
$("search").addEventListener("input", (e) => {
  clearTimeout(searchTimer);
  searchTimer = window.setTimeout(async () => {
    const q = (e.target as HTMLInputElement).value;
    const hits = await invoke<SearchHit[]>("search", { query: q });
    const ul = $("results");
    ul.innerHTML = "";
    for (const h of hits) {
      const li = document.createElement("li");
      li.innerHTML = `<span class="tag ${h.kind}">${h.kind}</span> ${esc(h.name)} <span class="muted">${esc(h.detail)}</span>`;
      li.onclick = () => invoke("show_item", { kind: h.kind, id: h.id });
      ul.append(li);
    }
  }, 150);
});

listen<{ text: string; matches: string[]; took_ms: number; audio_ms: number; retried: boolean }>("transcript", ({ payload }) => {
  const timing = `${(payload.audio_ms / 1000).toFixed(1)} s audio in ${(payload.took_ms / 1000).toFixed(1)} s${payload.retried ? " · unclear, decoded twice" : ""}`;
  addLog(`${highlight(payload.text, payload.matches)} <span class="muted">${timing}</span>`, payload.matches.length ? "hit" : "");
});
listen<ShowPayload>("show-item", ({ payload }) => {
  const name = payload.card?.name ?? payload.relic?.name ?? payload.id;
  addLog(`→ showing <b>${esc(name)}</b> (${payload.kind})`, "show");
});
listen<string>("status", ({ payload }) => addLog(esc(payload), "sys"));
listen<ListenState>("listen-state", ({ payload }) => renderListen(payload));
listen<number>("level", ({ payload }) => {
  $("level").style.width = `${Math.min(100, Math.sqrt(payload) * 300)}%`;
});
listen<[number, number | null]>("model-progress", ({ payload: [done, total] }) => {
  if (phase !== "starting") return; // late event from a cancelled download
  progress.hidden = false;
  if (total) {
    progress.max = total;
    progress.value = done;
  }
  listenStatus.textContent = `Downloading model… ${(done / 1e6).toFixed(0)}${total ? ` / ${(total / 1e6).toFixed(0)}` : ""} MB`;
});
listen<[number, number, number]>("prefetch", ({ payload: [done, total, failed] }) => {
  const pill = $("data-status");
  pill.title = `Images cached: ${done}/${total}${failed ? ` (${failed} failed, retried on demand)` : ""}`;
});
listen("data-ready", () => refreshStatus());

(async () => {
  settings = await invoke<Settings>("get_settings");
  fillForm(settings);
  await refreshStatus();
  await loadDevices();
})();
