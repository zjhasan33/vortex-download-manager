import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { invoke } from "@tauri-apps/api/core";

/** Create the floating drop box if enabled (and missing), or close it if disabled. */
export async function syncDropbox(enabled: boolean): Promise<void> {
  try {
    const existing = await WebviewWindow.getByLabel("dropbox");
    if (enabled) {
      if (existing) {
        try {
          await existing.show();
        } catch {
          /* already visible */
        }
        return;
      }
      const w = new WebviewWindow("dropbox", {
        url: "/#drop",
        title: "Vortex Drop Box",
        width: 250,
        height: 92,
        resizable: false,
        decorations: false,
        transparent: true,
        alwaysOnTop: true,
        skipTaskbar: true,
        focus: false,
      });
      w.once("tauri://error", (e) => console.error("dropbox failed", e));
    } else {
      try {
        await invoke("close_dropbox");
      } catch {}
      // Robust close: getByLabel may miss a just-created window, so also try direct close.
      if (existing) {
        try {
          await existing.close();
        } catch {}
      }
      try {
        const { getAllWebviewWindows } = await import("@tauri-apps/api/webviewWindow");
        const all = await getAllWebviewWindows();
        for (const w of all) {
          if (w.label === "dropbox") {
            try { await w.close(); } catch {}
          }
        }
      } catch {}
    }
  } catch (e) {
    console.error("syncDropbox", e);
  }
}
