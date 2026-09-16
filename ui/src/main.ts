import { initApi, store, startTicker } from "./lib/api";
import { api } from "./lib/api";
import { VortexApp } from "./app";
import { toast } from "./lib/ui";
import { bootDropbox } from "./dropbox";
import { syncDropbox } from "./lib/dropbox";

async function boot() {
  if (window.location.hash === "#drop") {
    await bootDropbox();
    return;
  }
  await initApi();
  await store.refresh();
  try {
    store.settings = await api.getSettings();
  } catch {
    /* settings optional on first run */
  }
  const app = new VortexApp();
  app.mount();
  startTicker(3);
  if (store.settings?.show_dropbox) void syncDropbox(true);

  window.__app = app;
  window.addEventListener("error", (e) => toast("UI error: " + e.message, "err"));
}

declare global {
  interface Window {
    __app: unknown;
  }
}

boot().catch((e) => console.error(e));