import { getCurrentWindow } from "@tauri-apps/api/window";
import { api, initApi, store } from "./lib/api";

/** Floating always-on-top drop target (opened as the "dropbox" window with #drop). */
export async function bootDropbox() {
  await initApi();
  try {
    store.settings = await api.getSettings();
  } catch {
    /* defaults */
  }
  const settings = () => store.settings;

  document.body.innerHTML = `
    <div id="dropbox">
      <div class="db-logo">▼</div>
      <div class="db-text" id="db-text">Drop links here</div>
      <button class="db-x" id="db-x" title="Close drop box">✕</button>
    </div>`;
  document.body.classList.add("dropwin");

  const say = (t: string, ms = 1800) => {
    const el = document.getElementById("db-text")!;
    el.textContent = t;
    window.setTimeout(() => (el.textContent = "Drop links here"), ms);
  };

  document.addEventListener("dragover", (e) => {
    e.preventDefault();
    document.getElementById("dropbox")!.classList.add("over");
  });
  document.addEventListener("dragleave", (e) => {
    if (e.relatedTarget === null) document.getElementById("dropbox")!.classList.remove("over");
  });
  document.addEventListener("drop", async (e) => {
    e.preventDefault();
    document.getElementById("dropbox")!.classList.remove("over");
    const txt =
      e.dataTransfer?.getData("text/uri-list") ||
      e.dataTransfer?.getData("URL") ||
      e.dataTransfer?.getData("text/plain") ||
      "";
    const urls = txt
      .split(/[\s,;]+/)
      .map((u) => u.trim())
      .filter((u) => u.startsWith("http://") || u.startsWith("https://"));
    if (!urls.length) {
      say("No links found");
      return;
    }
    let n = 0;
    for (const u of urls.slice(0, 50)) {
      try {
        await api.startDownload(u, settings()?.path || "", settings()?.segments ?? 16);
        n++;
      } catch {
        /* keep going */
      }
    }
    say(`${n} queued`);
  });

  document.getElementById("db-x")!.onclick = async (e) => {
    e.stopPropagation();
    try {
      await getCurrentWindow().close();
    } catch {
      /* ignore */
    }
  };
  document.getElementById("dropbox")!.onclick = () => {
    void api.windowAction("show");
  };
}
