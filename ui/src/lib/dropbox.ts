import { WebviewWindow } from "@tauri-apps/api/webviewWindow";

/** Create the floating drop box if enabled (and missing), or close it if disabled. */
export async function syncDropbox(enabled: boolean): Promise<void> {
  try {
    const existing = WebviewWindow.getByLabel("dropbox");
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
    } else if (existing) {
      try {
        await existing.close();
      } catch {
        /* already closed */
      }
    }
  } catch (e) {
    console.error("syncDropbox", e);
  }
}
