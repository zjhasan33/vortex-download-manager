import { openModal, toast } from "../lib/ui";
import { icon } from "../lib/icons";
import { api, store } from "../lib/api";
import { openIntercept } from "./modals";
import { formatBytes, formatDuration, formatNumber } from "../lib/format";
import type { YtdlInfo } from "../types";

export function openYoutube(preset?: { url?: string; playlist?: boolean; analyze?: boolean }) {
  let info: YtdlInfo | null = null;
  let selected: string | null = null;
  let loading = false;
  let subSel = "";
  let subFmt = "srt";
  // True when the chosen subtitle track is auto-generated: the backend must
  // fetch auto captions (otherwise an (auto) pick silently embeds nothing).
  let subAuto = false;
  let autoChecked = false;

  const close = openModal(
    () => `
  <div class="modal yt-modal">
    <div class="modal-head">
      <span style="color:#ff4d4d">${icon("youtube", 20)}</span>
      <h3>Download Video</h3>
      <div class="spacer"></div>
      <button class="x" data-close>${icon("close", 16)}</button>
    </div>
    <div class="modal-body">
      <div class="field">
        <label>Video URL (YouTube, Vimeo, TikTok, etc.)</label>
        <div style="display:flex;gap:8px">
          <input class="input" id="yt-url" placeholder="https://www.youtube.com/watch?v=..." spellcheck="false" />
          <button class="tbtn primary" id="yt-fetch">Analyze</button>
        </div>
      </div>
      <div id="yt-body">
        <div class="center-box"><span id="yt-msg">Paste a link and click Analyze.</span></div>
      </div>
      <div class="field" id="yt-pathwrap" style="display:none">
        <label>Save to folder</label>
        <div style="display:flex;gap:8px">
          <input class="input ext" id="yt-path" />
          <button class="tbtn" id="yt-browse">${icon("folder", 15)}</button>
        </div>
      </div>
      <div class="row2" id="yt-optrow" style="display:none">
        <div class="field">
          <label>Options</label>
          <div style="display:flex;gap:16px;padding-top:2px;flex-wrap:wrap">
            <label style="display:flex;gap:8px;align-items:center;font-size:12.5px;color:var(--text-2);cursor:pointer">
              <input type="checkbox" id="yt-playlist" /> Download whole playlist
            </label>
            <input class="input" id="yt-items" placeholder="Items (e.g. 1-10)" style="width:130px;display:none" />
            <label style="display:flex;gap:8px;align-items:center;font-size:12.5px;color:var(--text-2);cursor:pointer">
              <input type="checkbox" id="yt-embed" ${store.settings?.embed_subs !== false ? "checked" : ""} /> Embed official subtitles
            </label>
          </div>
          <div id="yt-plhint" style="font-size:11.5px;color:var(--text-3);padding-top:2px"></div>
        </div>
        <div class="field">
          <label>Start at (optional)</label>
          <input class="input" id="yt-start" type="datetime-local" />
        </div>
      </div>
    </div>
    <div class="modal-foot" id="yt-foot" style="display:none">
      <button class="btn-ghost" data-close>Cancel</button>
      <button class="tbtn primary" id="yt-go">${icon("download", 15)} Start Download</button>
    </div>
  </div>`,
    (root, close) => {
      root.querySelectorAll("[data-close]").forEach((b) => ((b as HTMLElement).onclick = close));
      const urlInp = root.querySelector<HTMLInputElement>("#yt-url")!;
      const body = root.querySelector<HTMLElement>("#yt-body")!;
      const pathwrap = root.querySelector<HTMLElement>("#yt-pathwrap")!;
      const optrow = root.querySelector<HTMLElement>("#yt-optrow")!;
      const foot = root.querySelector<HTMLElement>("#yt-foot")!;
      const pathInp = root.querySelector<HTMLInputElement>("#yt-path")!;
      const itemsInp = root.querySelector<HTMLInputElement>("#yt-items")!;
      const plChk = root.querySelector<HTMLInputElement>("#yt-playlist")!;
      // Destination hint so a flat single download from a playlist URL is
      // never a surprise (folder forms in playlist mode only).
      const syncPlHint = () => {
        const hint = root.querySelector<HTMLElement>("#yt-plhint");
        if (!hint) return;
        if (plChk.checked && info?.playlist) {
          const n = info.playlist_count ? `${info.playlist_count} videos` : "playlist";
          const t = info.playlist_title ? ` → folder "${info.playlist_title}"` : " → playlist folder";
          hint.textContent = `→ ${n}${t}`;
        } else if (info?.playlist) {
          hint.textContent = "→ Single video → Downloads root (no folder)";
        } else {
          hint.textContent = "";
        }
      };
      plChk.onchange = () => {
        itemsInp.style.display = plChk.checked ? "" : "none";
        syncPlHint();
      };

      const renderBody = () => {
        if (loading || !info) return;
        const best = info.formats.find((f) => f.note?.includes("Best"));
        const start = selected ?? best?.id ?? info.formats[0]?.id;
        if (start) selected = start;
        if (!subSel && info.subtitles.length) {
          subSel = info.subtitles[0].lang;
          subAuto = !!info.subtitles[0].auto;
        }

        body.innerHTML = `
          <div class="yt-info">
            <div class="yt-thumb" style="background-image:url('${info.thumbnail.replace(/'/g, "%27")}')"></div>
            <div class="yt-meta">
              <h4>${esc(info.title)}</h4>
              <div class="tags">
                ${info.uploader ? `<span class="tag">👤 ${esc(info.uploader)}</span>` : ""}
                <span class="tag">⏱ ${formatDuration(info.duration)}</span>
                ${info.view_count ? `<span class="tag">👁 ${formatNumber(info.view_count)}</span>` : ""}
                ${info.playlist ? `<span class="tag" style="background:var(--accent);color:#04121f">▶ ${info.playlist_count ? info.playlist_count + " videos" : "Playlist"}</span>` : ""}
              </div>
            </div>
          </div>
          <div class="field">
            <label>Choose format</label>
            <div class="yt-formats">${listFormats(info, selected!)}</div>
          </div>
          ${info.subtitles.length ? `
          <div class="field">
            <label>Subtitles / Captions</label>
            <div class="sub-select">
              <select class="input" id="yt-sublang">
                <option value="all" ${subSel === "all" ? "selected" : ""}>All languages</option>
                ${info.subtitles.map((s) => `<option value="${s.lang}" data-auto="${s.auto ? "1" : ""}" ${s.lang === subSel ? "selected" : ""}>${esc(s.label)}</option>`).join("")}
              </select>
              <select class="input" id="yt-subfmt" style="flex:0 0 96px;width:96px">
                ${["srt", "vtt"].map((f) => `<option value="${f}" ${f === subFmt ? "selected" : ""}>${f.toUpperCase()}</option>`).join("")}
              </select>
              <button class="tbtn primary" id="yt-subs-go">Download subs</button>
            </div>
          </div>` : ""}`;

        body.querySelectorAll<HTMLElement>(".fmt").forEach((el) => {
          el.onclick = () => {
            body.querySelectorAll(".fmt").forEach((f) => f.classList.remove("selected"));
            el.classList.add("selected");
            selected = el.dataset.id!;
            refreshGo();
          };
        });
        const subSelect = root.querySelector<HTMLSelectElement>("#yt-sublang");
        if (subSelect)
          subSelect.onchange = () => {
            subSel = subSelect.value;
            subAuto = subSelect.selectedOptions[0]?.dataset.auto === "1";
          };
        const subFmtSel = root.querySelector<HTMLSelectElement>("#yt-subfmt");
        if (subFmtSel) subFmtSel.onchange = () => (subFmt = subFmtSel.value);
        const subGo = root.querySelector<HTMLButtonElement>("#yt-subs-go");
        if (subGo)
          subGo.onclick = async () => {
            const lang = subSel || "all";
            await goDownload(`subs:${subFmt}:${lang}`);
          };

        pathInp.value = store.settings?.path ?? "";
        pathwrap.style.display = "";
        optrow.style.display = "";
        foot.style.display = "";
        // Whole-playlist links arrive pre-checked and show the item-range box.
        if (info.playlist && !autoChecked) {
          autoChecked = true;
          plChk.checked = true;
          itemsInp.style.display = "";
        }
        syncPlHint();
        refreshGo();
      };

      const refreshGo = () => {
        root.querySelector<HTMLButtonElement>("#yt-go")!.disabled = !selected;
      };

      root.querySelector<HTMLButtonElement>("#yt-fetch")!.onclick = async () => {
        const url = urlInp.value.trim();
        if (!url) return toast("Paste a video URL", "err");
        loading = true;
        selected = null;
        pathwrap.style.display = "none";
        optrow.style.display = "none";
        foot.style.display = "none";
        body.innerHTML = `<div class="center-box"><div class="spinner"></div><span>Analyzing video, please wait…</span></div>`;
        try {
          info = await api.fetchYtdlInfo(url);
          const best = info.formats.find((f) => f.note?.includes("Best"));
          selected = best?.id ?? info.formats[0]?.id ?? null;
          loading = false;
          renderBody();
        } catch (e: unknown) {
          loading = false;
          body.innerHTML = `<div class="center-box" style="color:var(--bad)">Failed to analyze: ${esc(String(e))}</div>`;
        }
      };

      root.querySelector<HTMLButtonElement>("#yt-browse")!.onclick = async () => {
        const p = await api.chooseFolder();
        if (p) pathInp.value = p;
      };

      urlInp.addEventListener("keydown", (e) =>
        e.key === "Enter" && root.querySelector<HTMLButtonElement>("#yt-fetch")!.click(),
      );

      const goDownload = async (formatId: string) => {
        try {
          const playlist = root.querySelector<HTMLInputElement>("#yt-playlist")!.checked;
          const items = root.querySelector<HTMLInputElement>("#yt-items")!.value.trim();
          const startInp = root.querySelector<HTMLInputElement>("#yt-start")!;
          const when = startInp.value ? new Date(startInp.value).getTime() || undefined : undefined;
          const embed = root.querySelector<HTMLInputElement>("#yt-embed")?.checked ?? true;
          const subLangs = subSel && subSel !== "all" ? subSel : store.settings?.sub_langs || "all";
          // Dialog mode (default ON): confirm with prefill; scheduled starts
          // keep today's direct path untouched.
          if (!when && (store.settings?.show_download_info ?? true)) {
            const fmt = (info?.formats || []).find((f) => f.id === formatId);
            const isSubs = formatId.startsWith("subs:");
            close();
            openIntercept(urlInp.value.trim(), isSubs ? undefined : info?.title, undefined, undefined, {
              thumbnail: info?.thumbnail,
              title: info?.title,
              size: fmt?.size || 0,
              category: "video",
              format: isSubs ? "SRT" : fmt?.ext?.toUpperCase(),
              saveDir: pathInp.value.trim() || undefined,
              isYtdl: true,
              ytdl: {
                format_id: formatId,
                playlist,
                playlist_items: items,
                embed_subs: embed,
                sub_langs: subLangs,
                embed_thumbnail: store.settings?.embed_thumbnail !== false,
                auto_subs: subAuto,
              },
            });
            return;
          }
          await api.startYtdl(
            urlInp.value.trim(),
            formatId,
            pathInp.value.trim(),
            playlist,
            items,
            when,
            embed,
            subLangs,
            store.settings?.embed_thumbnail !== false,
            subAuto,
            undefined,
            undefined,
            undefined,
            undefined,
            undefined,
            info?.thumbnail,
          );
          const msg = formatId.startsWith("subs:")
            ? "Subtitle download started"
            : formatId.startsWith("ba-") || formatId.startsWith("bestaudio")
              ? "Audio download started"
              : "Video download started";
          toast(msg, "ok");
          close();
        } catch (e: unknown) {
          toast("Download failed: " + String(e), "err");
        }
      };

      root.querySelector<HTMLButtonElement>("#yt-go")!.onclick = async () => {
        if (!selected) return;
        await goDownload(selected);
      };

      // Preset (e.g. a playlist URL copied to the clipboard): fill + analyze.
      if (preset?.url) {
        urlInp.value = preset.url;
        if (preset.playlist) {
          plChk.checked = true;
          itemsInp.style.display = "";
        }
        if (preset.analyze) setTimeout(() => root.querySelector<HTMLButtonElement>("#yt-fetch")!.click(), 80);
      }

      setTimeout(() => urlInp.focus(), 50);
    },
  );

  const listFormats = (inf: YtdlInfo, sel: string) =>
    inf.formats.map((f) => {
      const label = f.kind === "audio" ? "Audio" : "Video";
      const q = f.height ? `${f.height}p${f.fps ? "@" + f.fps : ""}` : f.quality;
      return `
        <div class="fmt ${f.id === sel ? "selected" : ""}" data-id="${f.id}">
          <span class="f-ico">${f.kind === "audio" ? "🎵" : "🎬"}</span>
          <span>
            <div class="f-lbl">${f.label}</div>
            <div class="f-note">${esc(label)} • ${esc(q)} • ${f.ext}</div>
          </span>
          ${f.note?.includes("Best") ? '<span class="pill best">BEST</span>' : ""}
          ${f.note?.includes("Recommended") && f.kind === "video" ? '<span class="pill recomm">REC</span>' : ""}
          <span class="f-size">${f.size ? formatBytes(f.size) : "~"}</span>
        </div>`;
    }).join("");

  const esc = (s: string) =>
    s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;").replace(/"/g, "&quot;");
}