import { openModal, toast } from "../lib/ui";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { icon } from "../lib/icons";
import { formatBytes } from "../lib/format";
import { api, store } from "../lib/api";
import { syncDropbox } from "../lib/dropbox";
import type { Settings, GrabItem } from "../types";

export function openAddUrl() {
  const segments = store.settings?.segments ?? 16;
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
        try {
          const urls = await api.readUrls();
          if (!urls.length) return toast("No URLs found", "err");
          let n = 0;
          for (const u of urls) {
            try {
              await api.startDownload(
                u,
                pathInp.value.trim() || store.settings?.path || "",
                Number(segInp.value),
                undefined,
              );
              n++;
            } catch (e: unknown) {
              toast(`Skipped ${u}: ${String(e)}`, "err");
            }
          }
          toast(`${n}/${urls.length} download(s) queued`, n ? "ok" : "err");
        } catch (e: unknown) {
          toast("Import failed: " + String(e), "err");
        }
        close();
      };

      const go = async () => {
        const url = urlInp.value.trim();
        if (!url) return (urlInp.style.borderColor = "var(--bad)");
        // Torrents are not supported in this version: magnet links and
        // .torrent files are rejected here instead of opening the P2P flow.
        if (/^magnet:/i.test(url)) {
          toast("Torrent downloads are not supported in this version", "err");
          return;
        }
        if (/\.torrent$/i.test(url) && !/^https?:/i.test(url)) {
          toast("Torrent downloads are not supported in this version", "err");
          return;
        }
        if (/^https?:.*\.torrent([?#]|$)/i.test(url)) {
          toast("Torrent downloads are not supported in this version", "err");
          return;
        }
        const fn = nameInp.value.trim() || url.split("/").pop() || `download_${Date.now()}`;
        const when = startInp.value ? new Date(startInp.value).getTime() || undefined : undefined;
        // Dialog mode (default ON): probe + confirm. Scheduled starts and
        // dialog-OFF keep today's direct path untouched.
        if (!when && (store.settings?.show_download_info ?? true)) {
          let info = { filename: fn, size: 0, category: "other" };
          try {
            info = await api.probeDownloadInfo(url);
          } catch (e: unknown) {
            toast("Probe failed: " + String(e), "err");
            return;
          }
          close();
          openIntercept(url, nameInp.value.trim() || info.filename, undefined, undefined, {
            size: info.size,
            category: info.category,
          });
          return;
        }
        const startWith = (mode?: string) =>
          api.startDownload(url, pathInp.value.trim() || store.settings?.path || "", Number(segInp.value), fn, when, undefined, mode);
        const doneOk = () => {
          toast(when ? "Scheduled" : "Download started", "ok");
          close();
        };
        try {
          await startWith("prompt");
          doneOk();
        } catch (e: unknown) {
          // IDM-style: file exists → ask Replace / Keep both / Cancel.
          const m = String(e).match(/^EXISTS::([\s\S]*)$/);
          if (!m) {
            toast(String(e), "err");
            return;
          }
          openConfirmExists(m[1], (choice) => {
            if (choice === "cancel") return;
            void startWith(choice === "replace" ? "replace" : undefined)
              .then(doneOk)
              .catch((e2: unknown) => toast(String(e2), "err"));
          });
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
        <label>Per-site proxy rules (first match wins; "DIRECT" bypasses global)</label>
        <div id="st-proxies" style="display:flex;flex-direction:column;gap:6px"></div>
        <div style="display:flex;gap:8px;padding-top:8px">
          <input class="input" id="st-px-domain" placeholder="github.com or *.example.com" spellcheck="false" style="flex:1" />
          <input class="input" id="st-px-url" placeholder="http://127.0.0.1:8080 / socks5://… / DIRECT" spellcheck="false" style="flex:1" />
          <button class="tbtn" id="st-px-add">Add</button>
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
        <label style="display:flex;gap:9px;align-items:center;cursor:pointer">
          <input type="checkbox" id="st-sched" ${s.sched_enabled ? "checked" : ""} /> Queue scheduler (daily start / stop)
        </label>
        <div class="row2" style="padding-top:8px">
          <div class="field">
            <label>Start queue at</label>
            <input class="input" id="st-sched-start" type="time" value="${escapeAttr(s.sched_start || "")}" />
          </div>
          <div class="field">
            <label>Stop queue at</label>
            <input class="input" id="st-sched-stop" type="time" value="${escapeAttr(s.sched_stop || "")}" />
          </div>
        </div>
        <div style="font-size:11px;color:var(--text-3);padding-top:4px">
          At start time everything paused resumes; at stop time active downloads pause. Times are daily (local). Overnight ranges like 22:00 → 06:00 work.
        </div>
      </div>
      <div class="field">
        <label>Options</label>
        <div style="display:grid;gap:8px;padding-top:2px">
          <label style="display:flex;gap:9px;align-items:center;font-size:12.5px;color:var(--text-2);cursor:pointer">
            <input type="checkbox" id="st-notif" ${s.notifications ? "checked" : ""} /> Notify on completion
          </label>
          <label style="display:flex;gap:9px;align-items:center;font-size:12.5px;color:var(--text-2);cursor:pointer">
            <input type="checkbox" id="st-sounds" ${s.sounds ?? true ? "checked" : ""} /> Notification sounds (completion / error chime)
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
          <label style="display:flex;gap:9px;align-items:center;font-size:12.5px;color:var(--text-2);cursor:pointer">
            <input type="checkbox" id="st-dlinfo" ${s.show_download_info ?? true ? "checked" : ""} /> Show "Download File Info" dialog before starting downloads
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
          <input class="input" id="st-sublangs" value="${escapeAttr(s.sub_langs)}" placeholder="all" style="width:130px" spellcheck="false" />
        </div>
        <div style="font-size:11px;color:var(--text-3);padding-top:4px">
          <b>all</b> = automatically embed any official language available (e.g. English, Bengali, Hindi). Use comma-separated codes like <b>en, bn</b> to restrict. ON = only manual/official subtitles are embedded (auto-generated captions are never used). OFF = video downloads with no subtitles at all. Down arrow <b>▾ Subs</b> in the extension downloads a standalone .srt/.vtt into the Subtitles folder.
        </div>
      </div>
      <div class="field">
        <label>YouTube</label>
        <div style="display:grid;gap:8px;padding-top:2px">
          <label style="display:flex;gap:9px;align-items:center;font-size:12.5px;color:var(--text-2);cursor:pointer">
            <input type="checkbox" id="st-thumb" ${s.embed_thumbnail ? "checked" : ""} /> Embed video thumbnail / album cover art
          </label>
        </div>
        <div style="font-size:11px;color:var(--text-3);padding-top:4px">
          Muxes the video thumbnail (MP4 / MKV cover track) or album cover art (MP3 ID3 tag) into every video and audio download.
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
      <button class="btn-ghost" id="st-abort-shutdown" title="Abort a pending Windows shutdown (shutdown /a)">Abort shutdown</button>
      <button class="tbtn primary" id="st-save">Save</button>
    </div>
  </div>`,
    (root, close) => {
      root.querySelectorAll("[data-close]").forEach((b) => ((b as HTMLElement).onclick = close));
      root.querySelector<HTMLButtonElement>("#st-abort-shutdown")!.onclick = async () => {
        try {
          await api.cancelShutdown();
          toast("Shutdown aborted", "ok");
        } catch (e: unknown) {
          toast("No shutdown to abort: " + String(e), "info");
        }
      };
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

      // Per-site proxy rules: working copy edited live, persisted on Save.
      type PxRule = { id: string; domain_pattern: string; proxy_url: string; enabled: boolean };
      let pxRules: PxRule[] = (s.per_site_proxies || []).map((r) => ({ ...r }));
      const pxBox = root.querySelector<HTMLElement>("#st-proxies")!;
      const renderPx = () => {
        pxBox.innerHTML = pxRules.length
          ? pxRules
              .map(
                (r) => `
            <div style="display:flex;align-items:center;gap:8px;background:rgba(255,255,255,0.04);border:1px solid rgba(255,255,255,0.07);border-radius:7px;padding:6px 8px">
              <input type="checkbox" data-px-on="${r.id}" ${r.enabled ? "checked" : ""} title="Enabled" style="flex:none" />
              <span style="flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:12px;color:var(--text-1)" title="${escapeAttr(r.domain_pattern)}">${escapeAttr(r.domain_pattern)}</span>
              <span style="flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap;font-size:11px;color:var(--text-3)" title="${escapeAttr(r.proxy_url)}">${escapeAttr(r.proxy_url)}</span>
              <button class="tbtn" data-px-del="${r.id}" title="Delete rule">${icon("close", 12)}</button>
            </div>`,
              )
              .join("")
          : '<span style="font-size:11.5px;color:var(--text-3)">No rules — everything uses the global proxy.</span>';
        pxBox.querySelectorAll<HTMLInputElement>("[data-px-on]").forEach((c) => {
          c.onchange = () => {
            const rule = pxRules.find((x) => x.id === c.dataset.pxOn);
            if (rule) rule.enabled = c.checked;
          };
        });
        pxBox.querySelectorAll<HTMLElement>("[data-px-del]").forEach((b) => {
          b.onclick = () => {
            pxRules = pxRules.filter((x) => x.id !== b.dataset.pxDel);
            renderPx();
          };
        });
      };
      renderPx();
      root.querySelector<HTMLButtonElement>("#st-px-add")!.onclick = () => {
        const dom = root.querySelector<HTMLInputElement>("#st-px-domain")!.value.trim();
        const url = root.querySelector<HTMLInputElement>("#st-px-url")!.value.trim();
        if (!dom) return toast("Enter a domain pattern", "err");
        if (!url) return toast("Enter a proxy URL or DIRECT", "err");
        const low = url.toLowerCase();
        const okUrl =
          low === "direct" ||
          low.startsWith("http://") ||
          low.startsWith("https://") ||
          low.startsWith("socks5://") ||
          low.startsWith("socks5h://");
        if (!okUrl) return toast("Proxy must be http(s)://, socks5(h):// or DIRECT", "err");
        pxRules.push({ id: `px_${Date.now()}`, domain_pattern: dom, proxy_url: url, enabled: true });
        root.querySelector<HTMLInputElement>("#st-px-domain")!.value = "";
        root.querySelector<HTMLInputElement>("#st-px-url")!.value = "";
        renderPx();
      };

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
          sounds: root.querySelector<HTMLInputElement>("#st-sounds")!.checked,
          auto_start: root.querySelector<HTMLInputElement>("#st-autostart")!.checked,
          categorize_folders: root.querySelector<HTMLInputElement>("#st-cat")!.checked,
          delete_part: s.delete_part,
          max_active: Math.max(1, Number(root.querySelector<HTMLInputElement>("#st-maxact")!.value) || 5),
          auto_retries: Math.max(0, Number(root.querySelector<HTMLInputElement>("#st-retries")!.value) || 0),
          proxy: root.querySelector<HTMLInputElement>("#st-proxy")!.value.trim(),
          per_site_proxies: pxRules,
          use_cookies: root.querySelector<HTMLInputElement>("#st-cookies")!.checked,
          cookies: ckPath.value.trim(),
          on_complete: root.querySelector<HTMLSelectElement>("#st-oncomplete")!.value,
          show_dropbox: root.querySelector<HTMLInputElement>("#st-dropbox")!.checked,
          clipboard_monitor: root.querySelector<HTMLInputElement>("#st-clip")!.checked,
          show_download_info: root.querySelector<HTMLInputElement>("#st-dlinfo")!.checked,
          category_paths: s.category_paths || {},
          embed_subs: root.querySelector<HTMLInputElement>("#st-embed")!.checked,
          sub_langs: root.querySelector<HTMLInputElement>("#st-sublangs")!.value.trim() || "all",
          embed_thumbnail: root.querySelector<HTMLInputElement>("#st-thumb")!.checked,
          credentials: s.credentials || [],
          stop_at: (() => {
            const v = root.querySelector<HTMLInputElement>("#st-stopat")!.value;
            return v ? new Date(v).getTime() || null : null;
          })(),
          sched_enabled: root.querySelector<HTMLInputElement>("#st-sched")!.checked,
          sched_start: root.querySelector<HTMLInputElement>("#st-sched-start")!.value.trim(),
          sched_stop: root.querySelector<HTMLInputElement>("#st-sched-stop")!.value.trim(),
        };
        try {
          await api.saveSettings(next);
          store.settings = next;
          void syncDropbox(next.show_dropbox).catch((e) => console.error("[dropbox sync]", e));
          toast("Settings saved", "ok");
          close();
        } catch (e: unknown) {
          toast("Save failed: " + String(e), "err");
        }
      };
    },
  );
}

export function openGrabber(initialUrl = "", autoStart = false) {
  // String-guard: the toolbar binds this as a click handler; never accept a DOM Event.
  if (typeof initialUrl !== "string") initialUrl = "";
  let items: GrabItem[] = [];
  let finding = false;
  let stopped = false;
  // Shared with the onClose hook below (chip must die with the modal).
  let chip: HTMLElement | null = null;

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
      <button class="x" id="gb-min" title="Minimize — watch downloads below">${icon("minimize", 16)}</button>
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
      <button class="tbtn danger" id="gb-stop" style="display:none">${icon("stop", 14)} Stop</button>
      <div style="flex:1"></div>
      <button class="btn-ghost" id="gb-cancel">Cancel</button>
      <button class="tbtn" id="gb-queue">${icon("pause", 14)} Add to Queue (Paused)</button>
      <button class="tbtn primary" id="gb-now">${icon("download", 15)} Download Now</button>
    </div>
  </div>`,
    (root, close) => {
      root.querySelectorAll("[data-close]").forEach((b) => ((b as HTMLElement).onclick = close));
      const urlInp = root.querySelector<HTMLInputElement>("#gb-url")!;
      const body = root.querySelector<HTMLElement>("#gb-body")!;
      const foot = root.querySelector<HTMLElement>("#gb-foot")!;
      const find = root.querySelector<HTMLButtonElement>("#gb-find")!;
      const stop = root.querySelector<HTMLButtonElement>("#gb-stop")!;
      const go = root.querySelector<HTMLButtonElement>("#gb-now")!;
      const que = root.querySelector<HTMLButtonElement>("#gb-queue")!;

      if (initialUrl) urlInp.value = initialUrl;

      const esc = (s: string) =>
        s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");

      // Minimize: hide the overlay so the download list below is visible and
      // pausable/stoppable, keep all modal state alive in this closure, and
      // show a floating chip to restore. The chip dies with the modal via the
      // openModal onClose hook (covers X, Cancel and outside-click).
      let submitted = 0;
      let submitTotal = 0;
      const chipLabel = () => {
        if (submitting) return `Grabber — starting ${submitted}/${submitTotal}… (click to restore)`;
        if (finding) return "Grabber — crawling… (click to restore)";
        if (items.length) return `Grabber — ${items.length} file(s) (click to restore)`;
        return "Grabber (click to restore)";
      };
      const syncChip = () => {
        if (chip) chip.querySelector("span")!.textContent = chipLabel();
      };
      const minimize = () => {
        if (chip) return;
        root.style.display = "none";
        chip = document.createElement("div");
        chip.title = "Restore Site Grabber";
        chip.style.cssText =
          "position:fixed;right:18px;bottom:70px;z-index:110;display:flex;gap:8px;align-items:center;" +
          "background:var(--bg-2);border:1px solid var(--border-strong);border-radius:12px;" +
          "padding:10px 14px;font-size:12.5px;color:var(--text-2);cursor:pointer;box-shadow:var(--shadow)";
        chip.innerHTML = `${icon("link", 14)}<span></span>`;
        chip.onclick = () => {
          chip?.remove();
          chip = null;
          root.style.display = "";
        };
        document.body.appendChild(chip);
        syncChip();
      };
      root.querySelector<HTMLButtonElement>("#gb-min")!.onclick = minimize;

      const setFinding = (on: boolean) => {
        finding = on;
        find.disabled = on;
        stop.style.display = on ? "" : "none";
        if (on) {
          foot.style.display = "flex";
          que.style.display = "none";
          go.style.display = "none";
        }
        syncChip();
      };

      const renderList = () => {
        stop.disabled = false;
        stop.innerHTML = `${icon("stop", 14)} Stop`;
        que.style.display = "";
        go.style.display = "";
        if (!items.length) {
          body.innerHTML = stopped
            ? `<div class="center-box"><span>Grab stopped — no files were found yet.</span></div>`
            : `<div class="center-box"><span>No downloadable files found. Try more pages or other types.</span></div>`;
          foot.style.display = "none";
          return;
        }
        body.innerHTML = `<div style="display:flex;justify-content:space-between;align-items:center;font-size:12px;color:var(--text-3);margin-bottom:8px">
            <span>${items.length} file(s) found</span>
            <label style="display:flex;gap:6px;align-items:center;color:var(--text-2);cursor:pointer"><input type="checkbox" id="gb-all" checked /> Select all</label>
          </div>
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
        foot.style.display = "flex";
        const all = root.querySelector<HTMLInputElement>("#gb-all")!;
        all.checked = true;
        all.onchange = () => body.querySelectorAll<HTMLInputElement>("input[data-idx]").forEach((c) => (c.checked = all.checked));
        syncChip();
      };

      stop.onclick = () => {
        stopped = true;
        stop.disabled = true;
        stop.innerHTML = `${icon("stop", 14)} Stopping…`;
        void api.grabStop();
      };

      const startGrab = async () => {
        const url = urlInp.value.trim();
        if (!url) return toast("Paste a page URL", "err");
        const sel: string[] = [];
        root.querySelectorAll<HTMLInputElement>("#gb-kinds input[data-kind]").forEach((c) => {
          if (c.checked) sel.push(c.dataset.kind!);
        });
        if (!sel.length) return toast("Select at least one file type", "err");
        const pages = Math.min(50, Math.max(1, Number(root.querySelector<HTMLInputElement>("#gb-pages")!.value) || 10));
        stopped = false;
        setFinding(true);
        body.innerHTML = `<div class="center-box"><div class="spinner"></div><span>Crawling pages, please wait…</span></div>`;
        try {
          items = await api.grabSite(url, pages, sel);
          setFinding(false);
          renderList();
          if (stopped) toast(items.length ? `Grab stopped — ${items.length} file(s) found so far` : "Grab stopped.", "info");
        } catch (e: unknown) {
          setFinding(false);
          renderList();
          if (!stopped) body.innerHTML = `<div class="center-box" style="color:var(--bad)">Grab failed: ${esc(String(e))}</div>`;
        }
      };

      find.onclick = () => {
        void startGrab();
      };

      urlInp.addEventListener("keydown", (e) =>
        e.key === "Enter" && root.querySelector<HTMLButtonElement>("#gb-find")!.click(),
      );

      // Bulk submit stays controllable: the modal does NOT auto-close, the
      // buttons show live progress, and Cancel turns into Stop so a 50-file
      // storm can be halted mid-flight (remaining items are skipped; use the
      // toolbar Stop for downloads that already started).
      let submitting = false;
      let stopSubmit = false;
      const cancelBtn = root.querySelector<HTMLButtonElement>("#gb-cancel")!;
      const submit = async (paused: boolean) => {
        if (submitting || finding) return;
        const checked = body.querySelectorAll<HTMLInputElement>("input[data-idx]:checked");
        if (!checked.length) return toast("Nothing selected", "err");
        const path = store.settings?.path ?? "";
        const segs = store.settings?.segments ?? 16;
        submitting = true;
        stopSubmit = false;
        find.disabled = true;
        que.style.display = "none";
        go.style.display = "none";
        cancelBtn.textContent = "Stop";
        let n = 0;
        const total = checked.length;
        submitTotal = total;
        submitted = 0;
        for (const c of checked) {
          if (stopSubmit) break;
          const it = items[Number(c.dataset.idx)];
          if (!it) continue;
          cancelBtn.textContent = `Stop (${n}/${total})`;
          try {
            await api.startDownload(it.url, path, segs, it.filename, undefined, paused);
            n++;
          } catch { /* keep going */ }
          submitted = n;
          syncChip();
        }
        submitting = false;
        find.disabled = false;
        que.style.display = "";
        go.style.display = "";
        cancelBtn.textContent = "Close";
        if (stopSubmit) {
          toast(`Stopped — ${n}/${total} started (toolbar Stop halts the rest)`, "info");
        } else {
          const cap = store.settings?.max_active ?? 5;
          toast(paused ? `${n} download(s) added — paused` : `${n} queued — ${cap} at once, rest wait`, "ok");
        }
      };
      que.onclick = () => void submit(true);
      go.onclick = () => void submit(false);
      // Replaces the plain closer bound above: while submitting, Cancel acts
      // as Stop for the remaining queue.
      cancelBtn.onclick = () => {
        if (submitting) {
          stopSubmit = true;
          cancelBtn.textContent = "Stopping…";
          return;
        }
        if (finding) void api.grabStop();
        close();
      };

      setTimeout(() => urlInp.focus(), 50);

      // Extension "Grab This Page": open the modal pre-filled AND start crawling
      // automatically so the user doesn't have to click "Find files" again.
      if (initialUrl && autoStart) {
        setTimeout(() => {
          void startGrab();
        }, 120);
      }
    },
    () => {
      // Modal truly gone (X / Cancel / outside-click): drop the minimize chip.
      chip?.remove();
      chip = null;
    },
  );
  void finding;
  return close;
}

function escapeAttr(s: string): string {
  return s.replace(/"/g, "&quot;").replace(/</g, "&lt;");
}

/** First-run welcome: tells a new user what happens automatically (tools),
 *  how to connect the browser, and how to start. Shown once (localStorage). */
export function openWelcome(toolsMissing: boolean, onSettings: () => void) {
  let busy = false;
  const close = openModal(
    () => `
  <div class="modal" style="width:520px">
    <div class="modal-head">
      <img src="/app-icon.png" alt="Vortex" style="width:26px;height:26px;border-radius:7px" />
      <h3>Welcome to Vortex</h3>
      <div class="spacer"></div>
      <button class="x" data-close>${icon("close", 16)}</button>
    </div>
    <div class="modal-body">
      <div style="display:grid;gap:12px;font-size:13px;color:var(--text-2)">
        <div style="display:flex;gap:10px;align-items:flex-start">
          <span style="font-size:16px">${toolsMissing ? "⬇️" : "✅"}</span>
          <span><b style="color:var(--text-1)">1. Helper tools</b><br/>${
            toolsMissing
              ? "yt-dlp + ffmpeg are downloading automatically — wait till both dots turn green."
              : "yt-dlp + ffmpeg are ready (green dots)."
          }</span>
        </div>
        <div style="display:flex;gap:10px;align-items:flex-start">
          <span style="font-size:16px">🔗</span>
          <span><b style="color:var(--text-1)">2. Browser extension</b><br/>Load the extension, then paste the pairing key from Settings so videos send here in one click.</span>
        </div>
        <div style="display:flex;gap:10px;align-items:flex-start">
          <span style="font-size:16px">⬇️</span>
          <span><b style="color:var(--text-1)">3. Download</b><br/>Add URL, YouTube, Grabber — or right-click a link in the browser.</span>
        </div>
      </div>
    </div>
      ${toolsMissing ? `<div id="wc-prog" style="display:none;margin-top:4px"><div style="height:6px;background:rgba(148,163,255,0.12);border-radius:999px;overflow:hidden"><div id="wc-fill" style="height:100%;background:linear-gradient(135deg,#22d3ee,#818cf8);width:0%;transition:width 0.3s"></div></div><span id="wc-label" style="font-size:11px;color:var(--text-3);font-family:var(--mono);margin-top:4px;display:block"></span></div>` : ""}
    </div>
    <div class="modal-foot">
      <button class="btn-ghost" data-close>Close</button>
      <button class="tbtn" id="wc-key">${icon("key", 14)} Copy pairing key</button>
      <button class="tbtn primary" id="wc-tools" ${toolsMissing ? "" : "disabled"}>${icon("download", 15)} Download tools now</button>
    </div>
  </div>`,
    (root, doClose) => {
      root.querySelectorAll("[data-close]").forEach((b) => ((b as HTMLElement).onclick = doClose));
      root.querySelector<HTMLButtonElement>("#wc-key")!.onclick = async () => {
        try {
          const key = await api.getWsToken();
          await navigator.clipboard.writeText(key);
          toast("Key copied — paste it in the extension popup, then open Settings", "ok");
          doClose();
          onSettings();
        } catch {
          toast("Copy failed — find the key in Settings", "err");
        }
      };
      const dl = root.querySelector<HTMLButtonElement>("#wc-tools")!;
      const progBox = root.querySelector<HTMLElement>("#wc-prog");
      const progFill = root.querySelector<HTMLElement>("#wc-fill");
      const progLabel = root.querySelector<HTMLElement>("#wc-label");
      let unlisten: (() => void) | null = null;
      if (progBox) {
        import("@tauri-apps/api/event").then(({ listen }) => {
          listen<{ kind: string; downloaded: number; total: number; progress: number; done?: boolean }>("tools-progress", (e) => {
            if (e.payload.done) {
              if (progLabel) progLabel.textContent = "Finishing…";
              if (progFill) progFill.style.width = "100%";
              return;
            }
            if (progBox) progBox.style.display = "";
            if (progFill) progFill.style.width = `${e.payload.progress || 0}%`;
            if (progLabel) progLabel.textContent = `${e.payload.kind} ${e.payload.progress || 0}% • ${Math.round((e.payload.downloaded || 0) / 1024 / 1024)}MB / ${Math.round((e.payload.total || 0) / 1024 / 1024) || "?"}MB`;
          }).then((fn) => (unlisten = fn));
        });
      }
      dl.onclick = async () => {
        if (busy) return;
        busy = true;
        dl.disabled = true;
        dl.innerHTML = `<span class="spin"></span> Downloading… (may take ~2 min)`;
        try {
          const res = await api.updateTools();
          store.tools = {
            ytdlp: res.ytdlp,
            ffmpeg: res.ffmpeg,
            ytdlp_version: res.ytdlp_version,
            ffmpeg_version: res.ffmpeg_version,
          };
          toast(res.ytdlp && res.ffmpeg ? "Tools ready — both dots green" : "Still missing something — try Update Tools again", res.ytdlp && res.ffmpeg ? "ok" : "err");
          if (unlisten) try { unlisten(); } catch {}
          doClose();
        } catch (e) {
          toast("Download failed: " + String(e), "err");
          dl.disabled = false;
          dl.textContent = "Retry download";
          busy = false;
        }
      };
    },
  );
  return close;
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

/** IDM-style "file already exists" choice: Replace / Keep both / Cancel. */
export function openConfirmExists(path: string, onChoice: (action: "replace" | "rename" | "cancel") => void) {
  const close = openModal(
    () => `
  <div class="modal" style="width:460px">
    <div class="modal-head">
      <h3>File already exists</h3>
      <div class="spacer"></div>
      <button class="x" data-close>✕</button>
    </div>
    <div class="modal-body">
      <div style="font-size:13px;color:var(--text-2);overflow:hidden;text-overflow:ellipsis;white-space:nowrap" title="${escapeAttr(path)}">${escapeAttr(path)}</div>
      <div style="font-size:12.5px;color:var(--text-3)">Replace overwrites it (partial files are discarded). Keep both saves as “name (1).ext”.</div>
    </div>
    <div class="modal-foot">
      <button class="btn-ghost" id="ce-cancel">Cancel</button>
      <button class="tbtn" id="ce-keep">Keep both</button>
      <button class="tbtn danger" id="ce-replace">Replace</button>
    </div>
  </div>`,
    (root, close) => {
      const done = (a: "replace" | "rename" | "cancel") => {
        onChoice(a);
        close();
      };
      root.querySelectorAll("[data-close]").forEach((b) => ((b as HTMLElement).onclick = () => done("cancel")));
      root.querySelector<HTMLButtonElement>("#ce-cancel")!.onclick = () => done("cancel");
      root.querySelector<HTMLButtonElement>("#ce-keep")!.onclick = () => done("rename");
      root.querySelector<HTMLButtonElement>("#ce-replace")!.onclick = () => done("replace");
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

/** IDM-style intercept: link clicked → Start Download / Download Later (paused) / Cancel. */
export interface DownloadInfoOpts {
  title?: string;
  size?: number;
  category?: string;
  format?: string;
  /** Pre-selected destination folder (e.g. picked in the YouTube modal). */
  saveDir?: string;
  isYtdl?: boolean;
  /** Called after every terminal close (Cancel/Later/Start/morph) — the
   *  standalone dialog window uses it to close itself. */
  onDone?: () => void;
  ytdl?: {
    format_id: string;
    playlist?: boolean;
    playlist_items?: string;
    embed_subs?: boolean;
    sub_langs?: string;
    embed_thumbnail?: boolean;
    auto_subs?: boolean;
  };
}

const INFO_CATS = [
  { id: "video", label: "Video" },
  { id: "audio", label: "Audio" },
  { id: "document", label: "Documents" },
  { id: "program", label: "Programs" },
  { id: "zip", label: "Archives" },
  { id: "other", label: "Other" },
];

/** Guess a category id from a filename (mirrors backend category_of). */
function catOfExt(name: string): string {
  const ext = (name.split(".").pop() || "").toLowerCase().split(/[^a-z0-9]/)[0];
  const map: Record<string, string[]> = {
    video: ["mp4", "mkv", "webm", "avi", "mov", "flv", "m4v", "wmv", "mpg", "mpeg", "3gp"],
    audio: ["mp3", "m4a", "aac", "flac", "wav", "ogg", "opus", "wma", "aiff"],
    document: ["pdf", "doc", "docx", "xls", "xlsx", "ppt", "pptx", "txt", "epub", "csv", "rtf", "odt"],
    program: ["exe", "msi", "apk", "appimage", "whl", "deb", "rpm", "bat", "cmd", "ps1"],
    zip: ["zip", "rar", "7z", "tar", "gz", "bz2", "xz", "iso", "dmg", "cab"],
  };
  for (const [cat, exts] of Object.entries(map)) if (exts.includes(ext)) return cat;
  return "other";
}

/** IDM-style "Download File Info" pre-download confirmation dialog. */
export function openIntercept(url: string, filename?: string, referer?: string, cookies?: string, opts?: DownloadInfoOpts) {
  const o = opts || {};
  // Standalone dialog window? Then this modal owns the window: custom
  // minimize/close + drag region, and onDone closes the window itself.
  let isDlg = false;
  try {
    isDlg = getCurrentWindow().label === "download-info";
  } catch {
    isDlg = false;
  }
  // YouTube jobs: force Video (or Audio) + MP4/MP3 badge — never "Other".
  const ytAudio = !!o.isYtdl && !!o.ytdl && /^(ba-|bestaudio)/.test(o.ytdl.format_id);
  const isYtUrl = /^(https?:\/\/)?(www\.|m\.)?(youtube\.com|youtu\.be)\//i.test(url);
  const rawFile = filename || url.split("/").pop() || url;
  // A raw query string ("watch?v=…") is not a filename: leave the input
  // empty (backend resolves the real name) with a clean placeholder.
  const queryish = !rawFile || /[?&=]/.test(rawFile) || !rawFile.includes(".");
  const file = queryish ? "" : rawFile;
  const name = o.title || (!queryish ? rawFile : "") || (o.isYtdl || isYtUrl ? "YouTube video" : url);
  const cat0 = o.isYtdl || isYtUrl ? (ytAudio ? "audio" : "video") : o.category || catOfExt(rawFile);
  const fmt = o.isYtdl || isYtUrl ? (ytAudio ? "MP3" : "MP4") : (o.format || (rawFile.split(".").pop() || "").split(/[^a-z0-9]/i)[0] || "file").toUpperCase().slice(0, 8);
  const remembered = store.settings?.category_paths?.[cat0];
  const saveTo = o.saveDir || remembered || store.settings?.path || "";
  const close = openModal(
    () => `
  <div class="modal" style="width:600px">
    <div class="modal-head"${isDlg ? ' data-tauri-drag-region style="cursor:move"' : ""}>
      <span style="color:var(--acc-1)">${icon("download", 18)}</span>
      <h3>Download File Info</h3>
      <div class="spacer"></div>
      ${isDlg
        ? `<div class="modal-window-controls">
             <button id="btn-info-minimize" class="btn-win-min" title="Minimize to taskbar/tray">—</button>
             <button id="btn-info-close" class="btn-win-close" title="Close">✕</button>
           </div>`
        : `<button class="x" data-close>${icon("close", 16)}</button>`}
    </div>
    <div class="modal-body">
      <div style="display:grid;grid-template-columns:1fr 148px;gap:14px">
        <div style="display:grid;gap:10px;min-width:0">
          <div class="field" style="margin:0">
            <label>URL</label>
            <div style="font-size:11.5px;color:var(--text-3);word-break:break-all;user-select:text">${escapeAttr(url)}</div>
          </div>
          <div class="field" style="margin:0">
            <label>Category</label>
            <select class="input" id="ic-cat">
              ${INFO_CATS.map((c) => `<option value="${c.id}" ${c.id === cat0 ? "selected" : ""}>${c.label}</option>`).join("")}
            </select>
          </div>
          <div class="field" style="margin:0">
            <label>Save As</label>
            <div style="display:flex;gap:8px">
              <input class="input ext" id="ic-path" value="${escapeAttr(saveTo)}" spellcheck="false" title="Destination folder" />
              <button class="tbtn" id="ic-browse" title="Browse">${icon("folder", 15)}</button>
            </div>
            <input class="input ext" id="ic-file" value="${escapeAttr(file)}" placeholder="YouTube Video (Auto-named on download)" spellcheck="false" title="File name" style="margin-top:6px" ${o.isYtdl ? "disabled" : ""} />
            <div id="ic-dupnote" style="display:none;font-size:11px;color:var(--warn,#fbbf24)"></div>
            ${o.isYtdl ? `<div style="font-size:11px;color:var(--text-3)">YouTube names the file from the video title.</div>` : ""}
          </div>
          <label style="display:flex;gap:9px;align-items:center;font-size:12px;color:var(--text-2);cursor:pointer">
            <input type="checkbox" id="ic-remember" /> Remember this path for this category
          </label>
        </div>
        <div style="display:flex;flex-direction:column;gap:8px;align-items:stretch">
          <div style="border-radius:12px;padding:14px 8px;text-align:center;background:linear-gradient(135deg,var(--acc-1),var(--acc-2));color:#04121f;font-weight:800;font-size:22px;letter-spacing:1px">${escapeAttr(fmt)}</div>
          <div style="text-align:center;font-size:12px;color:var(--text-2);font-family:var(--mono)" id="ic-size">${typeof o.size === "number" && o.size > 0 ? formatBytes(o.size) : "Calculating…"}</div>
          <div style="font-size:13px;color:var(--text-1);word-break:break-all;text-align:center">${escapeAttr(name)}</div>
        </div>
      </div>
    </div>
    <div class="modal-foot">
      <button class="btn-ghost" data-close>Cancel</button>
      <button class="tbtn" id="ic-later">Download Later</button>
      <button class="tbtn primary" id="ic-start">${icon("download", 15)} Start Download</button>
    </div>
  </div>`,
    (root, close) => {
      root.querySelectorAll("[data-close]").forEach((b) => ((b as HTMLElement).onclick = close));
      // Standalone dialog window controls (embedded mode keeps ✕ only).
      const minBtn = root.querySelector<HTMLButtonElement>("#btn-info-minimize");
      if (minBtn)
        minBtn.onclick = () => {
          getCurrentWindow()
            .minimize()
            .catch(() => {});
        };
      const dlgClose = root.querySelector<HTMLButtonElement>("#btn-info-close");
      if (dlgClose)
        dlgClose.onclick = () => {
          close();
          o.onDone?.();
        };
      root.querySelector<HTMLButtonElement>("#ic-browse")!.onclick = async () => {
        const p = await api.chooseFolder();
        if (p) root.querySelector<HTMLInputElement>("#ic-path")!.value = p;
      };
      // Category switch re-resolves the save folder (remembered > default).
      root.querySelector<HTMLSelectElement>("#ic-cat")!.onchange = () => {
        const cat = root.querySelector<HTMLSelectElement>("#ic-cat")!.value;
        const remembered = store.settings?.category_paths?.[cat];
        root.querySelector<HTMLInputElement>("#ic-path")!.value = remembered || store.settings?.path || "";
      };
      // Fill in real size for plain HTTP(S) links (YouTube passes its own).
      if (!o.isYtdl && (typeof o.size !== "number" || o.size <= 0)) {
        void api
          .probeDownloadInfo(url)
          .then((p) => {
            const el = root.querySelector<HTMLElement>("#ic-size");
            if (el && p.size > 0) el.textContent = formatBytes(p.size);
            else if (el) el.textContent = "Unknown";
          })
          .catch(() => {
            const el = root.querySelector<HTMLElement>("#ic-size");
            if (el) el.textContent = "Unknown";
          });
      }
      // YouTube duplicate note (informational only): yt-dlp auto-renames on
      // collision ("Name (1).ext"), so it can never silently overwrite —
      // but the user should still see it coming.
      if (o.isYtdl && o.title) {
        const ext = fmt === "MP3" ? "mp3" : "mp4";
        void api
          .ytdlExpectedPath(saveTo, o.title, ext, !!o.ytdl?.playlist)
          .then((r) => {
            if (!r.exists) return;
            const el = root.querySelector<HTMLElement>("#ic-dupnote");
            if (!el) return;
            el.style.display = "";
            el.textContent = r.is_dir
              ? "⚠ Folder already exists — new videos will be added alongside existing files."
              : `⚠ Already on disk — yt-dlp will save a numbered copy instead of overwriting.`;
          })
          .catch(() => {});
      }
      const rememberPath = () => {
        if (!root.querySelector<HTMLInputElement>("#ic-remember")!.checked) return;
        const cat = root.querySelector<HTMLSelectElement>("#ic-cat")!.value;
        const dir = root.querySelector<HTMLInputElement>("#ic-path")!.value.trim();
        if (!store.settings || !dir) return;
        const next = {
          ...store.settings,
          category_paths: { ...(store.settings.category_paths || {}), [cat]: dir },
        };
        store.settings = next;
        void api.saveSettings(next).catch((e) => console.error("[remember path]", e));
      };
      const go = async (later: boolean) => {
        const savePath = root.querySelector<HTMLInputElement>("#ic-path")!.value.trim() || store.settings?.path || "";
        const segs = store.settings?.segments ?? 16;
        rememberPath();
        // YouTube branch: yt-dlp names the file itself (filename input is
        // display-only there); start_paused queues for Download Later.
        // IDM parity: same file downloaded again must warn (Replace / Keep Both / Cancel) — never silently double.
        if (o.isYtdl && o.ytdl) {
          const y = o.ytdl;
          let ytdlAllowDup = false;
          // Pre-check: does the final file (or playlist folder) already exist on disk?
          if (!later && o.title) {
            try {
              const ext = /^(ba-|bestaudio)/.test(y.format_id) ? "mp3" : "mp4";
              const probe = await api.ytdlExpectedPath(savePath, o.title, ext, !!y.playlist);
              if (probe.exists) {
                const label = probe.path || o.title;
                const choice: "replace" | "rename" | "cancel" = await new Promise((res) => openConfirmExists(label, res));
                if (choice === "cancel") return;
                if (choice === "replace") {
                  try { await api.deleteFileAt(probe.path); } catch {}
                } else if (choice === "rename") {
                  ytdlAllowDup = true;
                }
              }
            } catch {}
          }
          let dl: { id: string } | null = null;
          try {
            dl = await api.startYtdl(
              url, y.format_id, savePath, y.playlist, y.playlist_items, undefined,
              y.embed_subs, y.sub_langs, y.embed_thumbnail, y.auto_subs,
              referer, undefined, cookies, later, ytdlAllowDup,
            );
          } catch (e: unknown) {
            const m = String(e).match(/^EXISTS::([\s\S]*)$/);
            if (m) {
              const choice: "replace" | "rename" | "cancel" = await new Promise((res) => openConfirmExists(m[1], res));
              if (choice === "cancel") return;
              if (choice === "replace") {
                try { await api.deleteFileAt(m[1].split(" (already")[0].trim()); } catch {}
              }
              try {
                dl = await api.startYtdl(url, y.format_id, savePath, y.playlist, y.playlist_items, undefined, y.embed_subs, y.sub_langs, y.embed_thumbnail, y.auto_subs, referer, undefined, cookies, later, choice === "rename");
              } catch (e2: unknown) {
                toast(String(e2), "err");
                return;
              }
            } else {
              toast(String(e), "err");
              return;
            }
          }
          if (later) {
            toast("Queued — will start later", "ok");
            close();
            return;
          }
          if (!dl) {
            close();
            return;
          }
          morphLive(dl.id);
          return;
        }
        // start_paused = true queues as Paused (Download Later).
        // NOTE: filename comes from the input only — never the raw URL
        // (a "watch?v=…" string must not become a file name).
        // The duplicate prompt fires for Later too (checked at queue time).
        const fileArg = () => root.querySelector<HTMLInputElement>("#ic-file")!.value.trim() || undefined;
        const start = (mode?: string) => api.startDownload(url, savePath, segs, fileArg(), undefined, later, mode ?? "prompt", referer, cookies);
        let dl: Awaited<ReturnType<typeof api.startDownload>> | null = null;
        try {
          dl = await start();
        } catch (e: unknown) {
          const m = String(e).match(/^EXISTS::([\s\S]*)$/);
          if (!m) {
            toast(String(e), "err");
            return;
          }
          const choice: "replace" | "rename" | "cancel" = await new Promise((res) => openConfirmExists(m[1], res));
          if (choice === "cancel") return;
          try {
            dl = await api.startDownload(url, savePath, segs, fileArg(), undefined, later, choice === "replace" ? "replace" : undefined, referer, cookies);
          } catch (e2: unknown) {
            toast(String(e2), "err");
            return;
          }
        }
        if (later) {
          toast("Queued — will start later", "ok");
          close();
          return;
        }
        if (!dl) {
          close();
          return;
        }
        // IDM-style: morph into live progress dialog (Pause/Cancel + Minimize to app)
        morphLive(dl.id);
      }
      function morphLive(id: string) {
        const body = root.querySelector<HTMLElement>(".modal-body")!;
        const foot = root.querySelector<HTMLElement>(".modal-foot")!;
        body.innerHTML = `
          <div style="display:grid;gap:10px">
            <div style="font-size:13px;color:var(--text-1);word-break:break-all">${escapeAttr(name)}</div>
            <div style="height:8px;background:rgba(148,163,255,0.12);border-radius:999px;overflow:hidden"><div id="ic-fill" style="height:100%;background:var(--grad);width:0%;transition:width 0.3s"></div></div>
            <div style="display:flex;justify-content:space-between;font-size:11px;color:var(--text-3);font-family:var(--mono)"><span id="ic-pct">0%</span><span id="ic-speed">–</span></div>
            <div id="ic-eta" style="font-size:11px;color:var(--text-3)"></div>
          </div>`;
        foot.innerHTML = `<button class="btn-ghost" id="ic-min">Minimize to app</button><button class="tbtn" id="ic-pause">${icon("pause", 14)} Pause</button><button class="tbtn danger" id="ic-cancel">${icon("close", 14)} Cancel</button>`;
        foot.querySelector<HTMLButtonElement>("#ic-min")!.onclick = close;
        const pauseBtn = foot.querySelector<HTMLButtonElement>("#ic-pause")!;
        const cancelBtn = foot.querySelector<HTMLButtonElement>("#ic-cancel")!;
        let paused = false;
        pauseBtn.onclick = async () => {
          try {
            if (paused) { await api.resumeDownload(id); paused = false; pauseBtn.innerHTML = `${icon("pause", 14)} Pause`; }
            else { await api.pauseDownload(id); paused = true; pauseBtn.innerHTML = `${icon("play", 14)} Resume`; }
          } catch (e: unknown) { toast(String(e), "err"); }
        };
        cancelBtn.onclick = async () => {
          try { await api.cancelDownload(id); } catch {}
          close();
        };
        const unsub = store.subscribe(() => {
          const d = store.downloads.find((x) => x.id === id);
          if (!d) return;
          const fill = root.querySelector<HTMLElement>("#ic-fill");
          const pctEl = root.querySelector<HTMLElement>("#ic-pct");
          const spEl = root.querySelector<HTMLElement>("#ic-speed");
          const etaEl = root.querySelector<HTMLElement>("#ic-eta");
          if (fill) fill.style.width = `${(d.progress || 0).toFixed(1)}%`;
          if (pctEl) pctEl.textContent = `${(d.progress || 0).toFixed(1)}% • ${formatBytes(d.downloaded)} / ${formatBytes(d.total_size || d.downloaded)}`;
          if (spEl) spEl.textContent = d.speed ? `${formatBytes(d.speed)}/s` : "–";
          if (etaEl) etaEl.textContent = d.eta ? `ETA ${Math.floor(d.eta / 60)}m ${d.eta % 60}s` : "";
          if (d.status === "completed") { toast("Download completed", "ok"); setTimeout(close, 900); unsub(); }
          if (d.status === "error" || d.status === "cancelled") { unsub(); }
        });
        // Also handle YouTube the same way — ytdlp tasks emit same store events
      }
      root.querySelector<HTMLButtonElement>("#ic-start")!.onclick = () => void go(false);
      root.querySelector<HTMLButtonElement>("#ic-later")!.onclick = () => void go(true);
    },
    () => o.onDone?.(),
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
