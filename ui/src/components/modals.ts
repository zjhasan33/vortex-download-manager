import { openModal, toast } from "../lib/ui";
import { icon } from "../lib/icons";
import { api, store } from "../lib/api";
import { syncDropbox } from "../lib/dropbox";
import type { Settings, GrabItem } from "../types";

export function openAddUrl() {
  const segments = store.settings?.segments ?? 8;
  const importBtn = `
      <div class="field">
        <label>Batch import (.txt)</label>
        <button class="tbtn" id="au-import">${icon("file", 14)} Import URL list…</button>
      </div>`;
  const close = openModal(
    () => `
  <div class="modal">
    <div class="modal-head">
      <span style="color:var(--acc-1)">${icon("link", 18)}</span>
      <h3>Add Download</h3>
      <div class="spacer"></div>
      <button class="x" data-close>${icon("close", 16)}</button>
    </div>
    <div class="modal-body">
      <div class="field">
        <label>File URL</label>
        <input class="input" id="au-url" placeholder="https://example.com/file.zip" spellcheck="false" />
      </div>
      <div class="row2">
        <div class="field">
          <label>Save to</label>
          <div style="display:flex;gap:8px">
            <input class="input ext" id="au-path" placeholder="Downloads" value="${store.settings?.path ?? ""}" />
            <button class="tbtn" id="au-browse" title="Browse">${icon("folder", 15)}</button>
          </div>
        </div>
        <div class="field">
          <label>Connections (segments)</label>
          <div class="seg-slider">
            <input type="range" id="au-seg" min="1" max="32" value="${segments}" />
            <span class="val" id="au-segval">${segments}</span>
          </div>
        </div>
      </div>
      <div class="field">
        <label>File name (optional)</label>
        <input class="input" id="au-name" placeholder="auto from URL" spellcheck="false" />
      </div>
      <div class="row2">
        <div class="field">
          <label>Start at (optional schedule)</label>
          <input class="input" id="au-start" type="datetime-local" />
        </div>
        <div class="field">${importBtn}</div>
      </div>
    </div>
    <div class="modal-foot">
      <button class="btn-ghost" data-close>Cancel</button>
      <button class="tbtn primary" id="au-go">${icon("download", 15)} Start Download</button>
    </div>
  </div>`,
    (root, close) => {
      root.querySelector<HTMLButtonElement>("[data-close]")!.onclick = close;
      root.querySelectorAll("[data-close]").forEach((b) => ((b as HTMLElement).onclick = close));

      const urlInp = root.querySelector<HTMLInputElement>("#au-url")!;
      const segInp = root.querySelector<HTMLInputElement>("#au-seg")!;
      const segVal = root.querySelector<HTMLSpanElement>("#au-segval")!;
      const pathInp = root.querySelector<HTMLInputElement>("#au-path")!;
      const nameInp = root.querySelector<HTMLInputElement>("#au-name")!;
      const startInp = root.querySelector<HTMLInputElement>("#au-start")!;

      segInp.addEventListener("input", () => (segVal.textContent = segInp.value));
      root.querySelector<HTMLButtonElement>("#au-browse")!.onclick = async () => {
        const p = await api.chooseFolder();
        if (p) pathInp.value = p;
      };
      root.querySelector<HTMLButtonElement>("#au-import")!.onclick = async () => {
        const urls = await api.readUrls();
        if (!urls.length) return toast("No URLs found", "err");
        for (const u of urls) {
          await api.startDownload(
            u,
            pathInp.value.trim() || store.settings?.path || "",
            Number(segInp.value),
            undefined,
          );
        }
        toast(`${urls.length} download(s) queued`, "ok");
        close();
      };

      const go = async () => {
        const url = urlInp.value.trim();
        if (!url) return (urlInp.style.borderColor = "var(--bad)");
        const fn = nameInp.value.trim() || url.split("/").pop() || `download_${Date.now()}`;
        const when = startInp.value ? new Date(startInp.value).getTime() || undefined : undefined;
        try {
          await api.startDownload(url, pathInp.value.trim() || store.settings?.path || "", Number(segInp.value), fn, when);
          toast(when ? "Scheduled" : "Download started", "ok");
          close();
        } catch (e: unknown) {
          toast(String(e), "err");
        }
      };
      urlInp.addEventListener("keydown", (e) => e.key === "Enter" && go());
      root.querySelector<HTMLButtonElement>("#au-go")!.onclick = go;
      setTimeout(() => urlInp.focus(), 50);
    },
  );
}

export function openSettings() {
  const s = store.settings;
  if (!s) return;
  const close = openModal(
    () => `
  <div class="modal">
    <div class="modal-head">
      <span style="color:var(--acc-2)">${icon("gear", 18)}</span>
      <h3>Settings</h3>
      <div class="spacer"></div>
      <button class="x" data-close>${icon("close", 16)}</button>
    </div>
    <div class="modal-body">
      <div class="row2">
        <div class="field">
          <label>Default download path</label>
          <div style="display:flex;gap:8px">
            <input class="input ext" id="st-path" value="${escapeAttr(s.path)}" />
            <button class="tbtn" id="st-browse">${icon("folder", 15)}</button>
          </div>
        </div>
        <div class="field">
          <label>Segments per download</label>
          <div class="seg-slider">
            <input type="range" id="st-seg" min="1" max="32" value="${s.segments}" />
            <span class="val" id="st-segval">${s.segments}</span>
          </div>
        </div>
      </div>
      <div class="row2">
        <div class="field">
          <label>Speed limit (KB/s, 0 = unlimited)</label>
          <input class="input" id="st-limit" type="number" min="0" value="${Math.round(s.speed_limit / 1024)}" />
        </div>
        <div class="field">
          <label>Max concurrent downloads</label>
          <input class="input" id="st-maxact" type="number" min="1" max="20" value="${s.max_active}" />
        </div>
      </div>
      <div class="row2">
        <div class="field">
          <label>Auto-retry attempts per download</label>
          <input class="input" id="st-retries" type="number" min="0" max="99" value="${s.auto_retries}" />
        </div>
        <div class="field">
          <label>Proxy (http/socks5, optional)</label>
          <input class="input" id="st-proxy" placeholder="http://127.0.0.1:7890" value="${escapeAttr(s.proxy)}" spellcheck="false" />
        </div>
      </div>
      <div class="field">
        <label>YouTube cookies (optional)</label>
        <div style="display:flex;gap:8px;align-items:center">
          <label style="display:flex;gap:9px;align-items:center;font-size:12.5px;color:var(--text-2);cursor:pointer;white-space:nowrap">
            <input type="checkbox" id="st-cookies" ${s.use_cookies ? "checked" : ""} /> Use cookies.txt
          </label>
          <input class="input ext" id="st-cookiepath" value="${escapeAttr(s.cookies)}" placeholder="path to exported cookies.txt" readonly />
          <button class="tbtn" id="st-cookiepick">${icon("folder", 15)}</button>
          <button class="tbtn" id="st-cookieclear" title="Clear">✕</button>
        </div>
        <div style="font-size:11px;color:var(--text-3);padding-top:4px">
          Toggle OFF = cookies always skipped (default). Toggle ON = yt-dlp uses your cookies.txt for YouTube, age-restricted & member-only videos.
        </div>
      </div>
      <div class="field">
        <label>Browser extension key (pairing)</label>
        <div style="display:flex;gap:8px;align-items:center">
          <input class="input ext" id="st-key" readonly placeholder="loading…" />
          <button class="tbtn" id="st-keycopy" title="Copy to clipboard">${icon("copy", 15)} Copy</button>
        </div>
        <div style="font-size:11px;color:var(--text-3);padding-top:4px">
          Copy this key into the Vortex browser extension → <b>Connection key</b> once, so only the extension can control the app.
        </div>
      </div>
      <div class="row2">
        <div class="field">
          <label>When all downloads complete</label>
          <select class="input" id="st-oncomplete">
            <option value="none" ${s.on_complete === "none" ? "selected" : ""}>Do nothing</option>
            <option value="shutdown" ${s.on_complete === "shutdown" ? "selected" : ""}>Shut down PC</option>
            <option value="sleep" ${s.on_complete === "sleep" ? "selected" : ""}>Sleep</option>
            <option value="hibernate" ${s.on_complete === "hibernate" ? "selected" : ""}>Hibernate</option>
            <option value="exit" ${s.on_complete === "exit" ? "selected" : ""}>Exit Vortex</option>
          </select>
        </div>
        <div class="field">
          <label>Stop downloads at (optional)</label>
          <input class="input" id="st-stopat" type="datetime-local" value="${s.stop_at ? toLocalInput(s.stop_at) : ""}" />
        </div>
      </div>
      <div class="field">
        <label>Options</label>
        <div style="display:grid;gap:8px;padding-top:2px">
          <label style="display:flex;gap:9px;align-items:center;font-size:12.5px;color:var(--text-2);cursor:pointer">
            <input type="checkbox" id="st-notif" ${s.notifications ? "checked" : ""} /> Notify on completion
          </label>
          <label style="display:flex;gap:9px;align-items:center;font-size:12.5px;color:var(--text-2);cursor:pointer">
            <input type="checkbox" id="st-cat" ${s.categorize_folders ? "checked" : ""} /> Sort into folders by type (Videos / Audio / etc.)
          </label>
          <label style="display:flex;gap:9px;align-items:center;font-size:12.5px;color:var(--text-2);cursor:pointer">
            <input type="checkbox" id="st-autostart" ${s.auto_start ? "checked" : ""} /> Start with Windows
          </label>
          <label style="display:flex;gap:9px;align-items:center;font-size:12.5px;color:var(--text-2);cursor:pointer">
            <input type="checkbox" id="st-dropbox" ${s.show_dropbox ? "checked" : ""} /> Show floating drop box
          </label>
          <label style="display:flex;gap:9px;align-items:center;font-size:12.5px;color:var(--text-2);cursor:pointer">
            <input type="checkbox" id="st-clip" ${s.clipboard_monitor ? "checked" : ""} /> Watch clipboard for URLs (auto-add like IDM)
          </label>
        </div>
      </div>
      <div class="field">
        <label>Subtitles</label>
        <div style="display:flex;gap:12px;align-items:center;flex-wrap:wrap">
          <label style="display:flex;gap:9px;align-items:center;font-size:12.5px;color:var(--text-2);cursor:pointer">
            <input type="checkbox" id="st-embed" ${s.embed_subs ? "checked" : ""} /> Embed official subtitles into video downloads
          </label>
          <span style="font-size:12.5px;color:var(--text-2)">Language(s)</span>
          <input class="input" id="st-sublangs" value="${escapeAttr(s.sub_langs)}" placeholder="en" style="width:130px" spellcheck="false" />
        </div>
        <div style="font-size:11px;color:var(--text-3);padding-top:4px">
          ON = only manual/official subtitles are embedded (auto-generated captions are never used). OFF = video downloads with no subtitles at all. Down arrow <b>▾ Subs</b> in the extension downloads a standalone .srt/.vtt into the Subtitles folder.
        </div>
      </div>
      <div class="field">
        <label>Site logins (HTTP 401 authentication)</label>
        <div id="st-creds" style="display:flex;flex-direction:column;gap:5px">
          ${(s.credentials || []).length === 0
            ? '<span style="font-size:11.5px;color:var(--text-3)">No saved logins. When a download needs a password, a login box appears automatically.</span>'
            : (s.credentials || [])
                .map(
                  (c) =>
                    `<div style="display:flex;align-items:center;gap:8px;background:rgba(255,255,255,0.04);border:1px solid rgba(255,255,255,0.07);border-radius:7px;padding:6px 8px">
                      <span style="flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:12px;color:var(--text-1)">${escapeAttr(c.host)}</span>
                      <span style="font-size:11px;color:var(--text-3)">${escapeAttr(c.username)}</span>
                      <button class="tbtn" data-cred-rm="${escapeAttr(c.host)}" title="Forget this login">${icon("close", 12)}</button>
                    </div>`,
                )
                .join("")}
        </div>
        <div style="font-size:11px;color:var(--text-3);padding-top:4px">
          Logins for a site are reused automatically on every download from that host. Supports Basic and Digest auth.
        </div>
      </div>
      <div class="field">
        <label>Tools &amp; Dependencies</label>
        <div id="st-tools" style="display:flex;flex-direction:column;gap:6px;align-items:stretch;max-width:340px"></div>
        <div style="font-size:11px;color:var(--text-3);padding-top:4px">
          yt-dlp needs frequent updates — YouTube keeps changing, so keeping it current prevents broken downloads. ffmpeg is used for merging and audio conversion.
        </div>
      </div>
    </div>
    <div class="modal-foot">
      <button class="btn-ghost" data-close>Cancel</button>
      <button class="tbtn primary" id="st-save">Save</button>
    </div>
  </div>`,
    (root, close) => {
      root.querySelectorAll("[data-close]").forEach((b) => ((b as HTMLElement).onclick = close));
      const pathInp = root.querySelector<HTMLInputElement>("#st-path")!;
      root.querySelector<HTMLButtonElement>("#st-browse")!.onclick = async () => {
        const p = await api.chooseFolder();
        if (p) pathInp.value = p;
      };
      const seg = root.querySelector<HTMLInputElement>("#st-seg")!;
      const segVal = root.querySelector<HTMLSpanElement>("#st-segval")!;
      seg.addEventListener("input", () => (segVal.textContent = seg.value));

      const ckPath = root.querySelector<HTMLInputElement>("#st-cookiepath")!;
      root.querySelector<HTMLButtonElement>("#st-cookiepick")!.onclick = async () => {
        const p = await api.chooseCookiesFile();
        if (p) ckPath.value = p;
      };
      root.querySelector<HTMLButtonElement>("#st-cookieclear")!.onclick = () => (ckPath.value = "");

      // Saved site logins: forget a login
      root.querySelectorAll<HTMLElement>("[data-cred-rm]").forEach((b) => {
        b.addEventListener("click", async () => {
          const host = b.dataset.credRm!;
          await api.removeCredential(host);
          store.settings = { ...store.settings!, credentials: (store.settings!.credentials || []).filter((c) => c.host !== host) };
          b.closest("div")?.remove();
          toast("Login forgotten for " + host, "ok");
        });
      });

      // Tools & Dependencies: status + update
      const toolsEl = root.querySelector<HTMLElement>("#st-tools")!;
      const renderTools = () => {
        const t = store.tools;
        const dot = (ok: boolean) => `<span class="dot ${ok ? "ok" : "missing"}"></span>`;
        const vs = (needle: string) =>
          t && (needle === "ytdlp" ? t.ytdlp_version : t.ffmpeg_version)
            ? "v" + (needle === "ytdlp" ? t.ytdlp_version : t.ffmpeg_version)
            : t && (needle === "ytdlp" ? t.ytdlp : t.ffmpeg)
              ? ""
              : "(not found)";
        toolsEl.innerHTML = `
          <div style="display:flex;align-items:center;gap:8px;font-size:12.5px;color:var(--text-2)">${dot(!!t?.ytdlp)} yt-dlp ${vs("ytdlp")}</div>
          <div style="display:flex;align-items:center;gap:8px;font-size:12.5px;color:var(--text-2)">${dot(!!t?.ffmpeg)} ffmpeg ${vs("ffmpeg")}</div>
          <button class="tbtn" id="st-update">↻ Update Tools</button>`;
        const btn = toolsEl.querySelector<HTMLButtonElement>("#st-update")!;
        btn.onclick = async () => {
          btn.disabled = true;
          btn.innerHTML = `<span class="spin"></span> Checking for updates…`;
          try {
            const res = await api.updateTools();
            store.tools = {
              ytdlp: res.ytdlp,
              ffmpeg: res.ffmpeg,
              ytdlp_version: res.ytdlp_version,
              ffmpeg_version: res.ffmpeg_version,
            };
            const msg =
              res.message ||
              (res.updated ? "yt-dlp successfully updated" : "Tools are already up to date!");
            toast(msg, res.message?.toLowerCase().includes("timed out") ? "err" : res.updated ? "ok" : "info");
          } catch (e) {
            toast("Update check failed: " + String(e), "err");
          } finally {
            // Always re-render the tools block so the spinner can never get stuck.
            renderTools();
          }
        };
      };
      renderTools();

      // Browser extension pairing key
      const keyInp = root.querySelector<HTMLInputElement>("#st-key")!;
      void api.getWsToken().then((t) => {
        keyInp.value = t;
      });
      root.querySelector<HTMLButtonElement>("#st-keycopy")!.onclick = async () => {
        try {
          await navigator.clipboard.writeText(keyInp.value);
          toast("Key copied — paste it in the extension popup", "ok");
        } catch {
          toast("Copy failed — select the key and press Ctrl+C", "err");
        }
      };

      root.querySelector<HTMLButtonElement>("#st-save")!.onclick = async () => {
        const next: Settings = {
          path: pathInp.value.trim() || s.path,
          segments: Number(seg.value),
          speed_limit: Math.max(0, Number(root.querySelector<HTMLInputElement>("#st-limit")!.value) * 1024),
          notifications: root.querySelector<HTMLInputElement>("#st-notif")!.checked,
          auto_start: root.querySelector<HTMLInputElement>("#st-autostart")!.checked,
          categorize_folders: root.querySelector<HTMLInputElement>("#st-cat")!.checked,
          delete_part: s.delete_part,
          max_active: Math.max(1, Number(root.querySelector<HTMLInputElement>("#st-maxact")!.value) || 5),
          auto_retries: Math.max(0, Number(root.querySelector<HTMLInputElement>("#st-retries")!.value) || 0),
          proxy: root.querySelector<HTMLInputElement>("#st-proxy")!.value.trim(),
          use_cookies: root.querySelector<HTMLInputElement>("#st-cookies")!.checked,
          cookies: ckPath.value.trim(),
          on_complete: root.querySelector<HTMLSelectElement>("#st-oncomplete")!.value,
          show_dropbox: root.querySelector<HTMLInputElement>("#st-dropbox")!.checked,
          clipboard_monitor: root.querySelector<HTMLInputElement>("#st-clip")!.checked,
          embed_subs: root.querySelector<HTMLInputElement>("#st-embed")!.checked,
          sub_langs: root.querySelector<HTMLInputElement>("#st-sublangs")!.value.trim() || "en",
          credentials: s.credentials || [],
          stop_at: (() => {
            const v = root.querySelector<HTMLInputElement>("#st-stopat")!.value;
            return v ? new Date(v).getTime() || null : null;
          })(),
        };
        await api.saveSettings(next);
        store.settings = next;
        void syncDropbox(next.show_dropbox);
        toast("Settings saved", "ok");
        close();
      };
    },
  );
}

export function openGrabber() {
  let items: GrabItem[] = [];
  let finding = false;

  const kinds: Array<[string, string, boolean]> = [
    ["video", "Videos", true],
    ["audio", "Audio", true],
    ["document", "Documents", true],
    ["archive", "Archives", true],
    ["image", "Images", false],
  ];

  const close = openModal(
    () => `
  <div class="modal" style="width:620px">
    <div class="modal-head">
      <span style="color:var(--acc-1)">${icon("link", 18)}</span>
      <h3>Site Grabber</h3>
      <div class="spacer"></div>
      <button class="x" data-close>${icon("close", 16)}</button>
    </div>
    <div class="modal-body">
      <div class="field">
        <label>Page URL to crawl (same site only)</label>
        <div style="display:flex;gap:8px">
          <input class="input" id="gb-url" placeholder="https://example.com/page" spellcheck="false" />
          <button class="tbtn primary" id="gb-find">Find files</button>
        </div>
      </div>
      <div class="row2">
        <div class="field">
          <label>File types</label>
          <div style="display:flex;gap:12px;flex-wrap:wrap;padding-top:2px" id="gb-kinds">
            ${kinds.map(([k, lbl, on]) => `<label style="display:flex;gap:6px;align-items:center;font-size:12.5px;color:var(--text-2);cursor:pointer"><input type="checkbox" data-kind="${k}" ${on ? "checked" : ""} /> ${lbl}</label>`).join("")}
          </div>
        </div>
        <div class="field">
          <label>Max pages to crawl</label>
          <input class="input" id="gb-pages" type="number" min="1" max="50" value="10" />
        </div>
      </div>
      <div id="gb-body">
        <div class="center-box"><span>Paste a page URL and click Find files.</span></div>
      </div>
    </div>
    <div class="modal-foot" id="gb-foot" style="display:none">
      <label style="display:flex;gap:8px;align-items:center;font-size:12.5px;color:var(--text-2);cursor:pointer;margin-right:auto">
        <input type="checkbox" id="gb-all" checked /> Select all
      </label>
      <button class="btn-ghost" data-close>Cancel</button>
      <button class="tbtn primary" id="gb-go">${icon("download", 15)} Download selected</button>
    </div>
  </div>`,
    (root, close) => {
      root.querySelectorAll("[data-close]").forEach((b) => ((b as HTMLElement).onclick = close));
      const urlInp = root.querySelector<HTMLInputElement>("#gb-url")!;
      const body = root.querySelector<HTMLElement>("#gb-body")!;
      const foot = root.querySelector<HTMLElement>("#gb-foot")!;

      const esc = (s: string) =>
        s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

      const renderList = () => {
        if (!items.length) {
          body.innerHTML = `<div class="center-box"><span>No downloadable files found. Try more pages or other types.</span></div>`;
          foot.style.display = "none";
          return;
        }
        body.innerHTML = `<div style="font-size:12px;color:var(--text-3);margin-bottom:8px">${items.length} file(s) found</div>
          <div class="yt-formats" style="max-height:300px;overflow-y:auto;display:grid;gap:6px">
          ${items.map((it, i) => `
            <label class="fmt" style="cursor:pointer">
              <input type="checkbox" data-idx="${i}" checked style="margin-right:10px" />
              <span>
                <div class="f-lbl">${esc(it.filename)}</div>
                <div class="f-note">${esc(it.kind)} • ${esc(it.url.length > 90 ? it.url.slice(0, 90) + "…" : it.url)}</div>
              </span>
            </label>`).join("")}
          </div>`;
        foot.style.display = "";
        const all = root.querySelector<HTMLInputElement>("#gb-all")!;
        all.checked = true;
        all.onchange = () => body.querySelectorAll<HTMLInputElement>("input[data-idx]").forEach((c) => (c.checked = all.checked));
      };

      root.querySelector<HTMLButtonElement>("#gb-find")!.onclick = async () => {
        const url = urlInp.value.trim();
        if (!url) return toast("Paste a page URL", "err");
        const sel: string[] = [];
        root.querySelectorAll<HTMLInputElement>("#gb-kinds input[data-kind]").forEach((c) => {
          if (c.checked) sel.push(c.dataset.kind!);
        });
        if (!sel.length) return toast("Select at least one file type", "err");
        const pages = Math.min(50, Math.max(1, Number(root.querySelector<HTMLInputElement>("#gb-pages")!.value) || 10));
        finding = true;
        foot.style.display = "none";
        body.innerHTML = `<div class="center-box"><div class="spinner"></div><span>Crawling pages, please wait…</span></div>`;
        try {
          items = await api.grabSite(url, pages, sel);
          finding = false;
          renderList();
        } catch (e: unknown) {
          finding = false;
          body.innerHTML = `<div class="center-box" style="color:var(--bad)">Grab failed: ${esc(String(e))}</div>`;
        }
      };

      urlInp.addEventListener("keydown", (e) =>
        e.key === "Enter" && root.querySelector<HTMLButtonElement>("#gb-find")!.click(),
      );

      root.querySelector<HTMLButtonElement>("#gb-go")!.onclick = async () => {
        const checked = body.querySelectorAll<HTMLInputElement>("input[data-idx]:checked");
        if (!checked.length) return toast("Nothing selected", "err");
        const path = store.settings?.path ?? "";
        const segs = store.settings?.segments ?? 8;
        let n = 0;
        for (const c of checked) {
          const it = items[Number(c.dataset.idx)];
          if (!it) continue;
          try {
            await api.startDownload(it.url, path, segs, it.filename);
            n++;
          } catch { /* keep going */ }
        }
        toast(`${n} download(s) queued`, "ok");
        close();
      };

      setTimeout(() => urlInp.focus(), 50);
    },
  );
  void finding;
  return close;
}

function escapeAttr(s: string): string {
  return s.replace(/"/g, "&quot;").replace(/</g, "&lt;");
}

export function openConfirmRemove(name: string, onYes: (deleteFile: boolean) => void) {
  const close = openModal(
    () => `
  <div class="modal" style="width:440px">
    <div class="modal-head">
      <h3>Remove download?</h3>
      <div class="spacer"></div>
      <button class="x" data-close>✕</button>
    </div>
    <div class="modal-body">
      <div style="font-size:13px;color:var(--text-2);overflow:hidden;text-overflow:ellipsis;white-space:nowrap">${escapeAttr(name)}</div>
      <label style="display:flex;gap:9px;align-items:center;font-size:12.5px;color:var(--text-2);cursor:pointer">
        <input type="checkbox" id="cf-delfile" /> Also delete the file from disk
      </label>
    </div>
    <div class="modal-foot">
      <button class="btn-ghost" data-close>Cancel</button>
      <button class="tbtn primary" id="cf-yes">Remove</button>
    </div>
  </div>`,
    (root, close) => {
      root.querySelectorAll("[data-close]").forEach((b) => ((b as HTMLElement).onclick = close));
      root.querySelector<HTMLButtonElement>("#cf-yes")!.onclick = () => {
        const del = root.querySelector<HTMLInputElement>("#cf-delfile")!.checked;
        onYes(del);
        close();
      };
    },
  );
  return close;
}

export function openConfirmBulkRemove(count: number, onYes: (deleteFile: boolean) => void) {
  const close = openModal(
    () => `
  <div class="modal" style="width:440px">
    <div class="modal-head">
      <h3>Remove ${count} download(s)?</h3>
      <div class="spacer"></div>
      <button class="x" data-close>✕</button>
    </div>
    <div class="modal-body">
      <div style="font-size:13px;color:var(--text-2)">This removes ${count} item(s) from the list.</div>
      <label style="display:flex;gap:9px;align-items:center;font-size:12.5px;color:var(--text-2);cursor:pointer">
        <input type="checkbox" id="cf-delfile" /> Also delete the file(s) from disk
      </label>
    </div>
    <div class="modal-foot">
      <button class="btn-ghost" data-close>Cancel</button>
      <button class="tbtn primary" id="cf-yes">Remove</button>
    </div>
  </div>`,
    (root, close) => {
      root.querySelectorAll("[data-close]").forEach((b) => ((b as HTMLElement).onclick = close));
      root.querySelector<HTMLButtonElement>("#cf-yes")!.onclick = () => {
        const del = root.querySelector<HTMLInputElement>("#cf-delfile")!.checked;
        onYes(del);
        close();
      };
    },
  );
  return close;
}

function toLocalInput(ms: number): string {
  const d = new Date(ms);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`;
}

/** Login prompt shown when a download hits HTTP 401/407. */
export function openAuthDialog(p: { id: string; url: string; host: string }) {
  const close = openModal(
    () => `
  <div class="modal" style="width:460px">
    <div class="modal-head">
      <span style="color:var(--acc-3)">${icon("key", 17)}</span>
      <h3>Login required</h3>
      <div class="spacer"></div>
      <button class="x" data-close>${icon("close", 16)}</button>
    </div>
    <div class="modal-body">
      <div style="font-size:12px;color:var(--text-2);margin-bottom:2px">
        This download requests a username & password:
      </div>
      <div class="field">
        <label>Site</label>
        <div class="auth-host">${escapeAttr(p.host)}</div>
      </div>
      <div class="field">
        <label>Username</label>
        <input class="input" id="au-user" autocomplete="off" spellcheck="false" style="font-family:var(--mono)" />
      </div>
      <div class="field">
        <label>Password</label>
        <input class="input" id="au-pass" type="password" autocomplete="off" spellcheck="false" style="font-family:var(--mono)" />
      </div>
      <label style="display:flex;gap:9px;align-items:center;font-size:12.5px;color:var(--text-2);cursor:pointer;padding-top:2px">
        <input type="checkbox" id="au-remember" checked /> Remember for this site (auto-login next time)
      </label>
      <div style="font-size:11px;color:var(--text-3);padding-top:6px">URL: <span class="auth-url">${escapeAttr(p.url)}</span></div>
    </div>
    <div class="modal-foot">
      <button class="btn-ghost" data-close>Cancel</button>
      <button class="tbtn primary" id="au-login">Login & retry</button>
    </div>
  </div>`,
    (root, doClose) => {
      root.querySelectorAll("[data-close]").forEach((b) => ((b as HTMLElement).onclick = doClose));
      const userInp = root.querySelector<HTMLInputElement>("#au-user")!;
      const passInp = root.querySelector<HTMLInputElement>("#au-pass")!;
      const rememberInp = root.querySelector<HTMLInputElement>("#au-remember")!;
      const loginBtn = root.querySelector<HTMLButtonElement>("#au-login")!;
      userInp.focus();
      const submit = async () => {
        const username = userInp.value.trim();
        const password = passInp.value;
        if (!username) {
          userInp.focus();
          return;
        }
        loginBtn.disabled = true;
        loginBtn.textContent = "Logging in…";
        try {
          await api.setAuth(p.id, p.host, username, password, rememberInp.checked);
          toast("Credentials saved — retrying download", "ok");
        } catch (e) {
          toast("Could not apply login: " + String(e), "err");
        }
        doClose();
      };
      loginBtn.onclick = submit;
      passInp.addEventListener("keydown", (e) => {
        if (e.key === "Enter") void submit();
      });
      userInp.addEventListener("keydown", (e) => {
        if (e.key === "Enter") passInp.focus();
      });
    },
  );
  return close;
}