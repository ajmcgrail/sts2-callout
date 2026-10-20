// Slay the Spire 2 text uses BBCode-ish tags: [gold]Vulnerable[/gold], [energy:2], [star:1] ...

const COLORS = new Set(["gold", "red", "blue", "green", "purple", "orange", "aqua", "pink"]);

function escapeHtml(s: string): string {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
}

export function renderMarkup(text: string | null | undefined): string {
  if (!text) return "";
  let depth = 0;
  const html = escapeHtml(text).replace(/\[(\/?)([a-z_]+)(?::([^\]]*))?\]/gi, (_m, close: string, tag: string, arg?: string) => {
    tag = tag.toLowerCase();
    if (tag === "energy") return `<span class="energy" title="Energy">${escapeHtml(arg ?? "")}</span>`;
    if (tag === "star") return `<span class="star" title="Star">${escapeHtml(arg ?? "")}</span>`;
    if (COLORS.has(tag) || tag === "b" || tag === "i") {
      if (close) {
        if (depth === 0) return "";
        depth--;
        return "</span>";
      }
      depth++;
      return `<span class="k-${tag}">`;
    }
    return ""; // effect tags like [sine], [jitter]
  });
  return html.replace(/\n/g, "<br>") + "</span>".repeat(depth);
}

/** Plain text, for tooltips. */
export function stripMarkup(text: string | null | undefined): string {
  return (text ?? "").replace(/\[energy:([^\]]*)\]/g, "$1 energy").replace(/\[[^\]]*\]/g, "");
}

export function titleCase(s: string): string {
  return s.replace(/\b\w/g, (c) => c.toUpperCase());
}
