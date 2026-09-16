import { icon } from "./icons";

let wrap: HTMLDivElement | null = null;

export function toast(msg: string, kind: "info" | "err" | "ok" = "info"): void {
  if (!wrap) {
    wrap = document.createElement("div");
    wrap.className = "toast-wrap";
    document.body.appendChild(wrap);
  }
  const t = document.createElement("div");
  t.className = `toast ${kind}`;
  t.innerHTML = `${icon(kind === "err" ? "close" : kind === "ok" ? "check" : "bell", 15)}
    <span>${escapeHtml(msg)}</span>`;
  wrap.appendChild(t);
  setTimeout(() => t.remove(), 3800);
}

export function escapeHtml(s: string): string {
  return s
    .replace(/&/g, "&amp;")
    .replace(/</g, "&lt;")
    .replace(/>/g, "&gt;")
    .replace(/"/g, "&quot;");
}

export function openModal(html: () => string, onMounted?: (root: HTMLElement, close: () => void) => void): () => void {
  const root = document.getElementById("modal-root")!;
  const overlay = document.createElement("div");
  overlay.className = "modal-overlay";
  overlay.innerHTML = html();
  root.appendChild(overlay);
  const close = () => overlay.remove();
  overlay.addEventListener("mousedown", (e) => {
    if (e.target === overlay) close();
  });
  if (onMounted) onMounted(overlay, close);
  return close;
}