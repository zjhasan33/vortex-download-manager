// Tiny synthesized notification chimes (Web Audio API, no assets).
// Fire-and-forget: scheduling is async by nature, so playback can never
// block the UI thread or the download engine. Everything is wrapped so a
// missing/blocked AudioContext fails silently.

let ctx: AudioContext | null = null;

function ac(): AudioContext | null {
  try {
    if (!ctx) ctx = new AudioContext();
    if (ctx.state === "suspended") void ctx.resume();
    return ctx;
  } catch {
    return null;
  }
}

function tone(
  c: AudioContext,
  freq: number,
  delay: number,
  dur: number,
  type: OscillatorType = "sine",
  vol = 0.12,
): void {
  const t0 = c.currentTime + delay;
  const osc = c.createOscillator();
  const g = c.createGain();
  osc.type = type;
  osc.frequency.value = freq;
  g.gain.setValueAtTime(0.0001, t0);
  g.gain.exponentialRampToValueAtTime(vol, t0 + 0.015);
  g.gain.exponentialRampToValueAtTime(0.0001, t0 + dur);
  osc.connect(g).connect(c.destination);
  osc.start(t0);
  osc.stop(t0 + dur + 0.05);
}

/** Clean two-tone success chime (E5 → A5). */
export function playSuccess(): void {
  try {
    const c = ac();
    if (!c) return;
    tone(c, 659.25, 0, 0.16);
    tone(c, 880.0, 0.11, 0.24);
  } catch { /* silent */ }
}

/** Soft low warning buzz (failed / unrecoverable error). */
export function playError(): void {
  try {
    const c = ac();
    if (!c) return;
    tone(c, 196.0, 0, 0.18, "triangle", 0.14);
    tone(c, 147.0, 0.14, 0.26, "triangle", 0.14);
  } catch { /* silent */ }
}

/** Little arpeggio when the whole queue finishes (C5 E5 G5 C6). */
export function playBatch(): void {
  try {
    const c = ac();
    if (!c) return;
    tone(c, 523.25, 0, 0.14);
    tone(c, 659.25, 0.1, 0.14);
    tone(c, 783.99, 0.2, 0.14);
    tone(c, 1046.5, 0.3, 0.3);
  } catch { /* silent */ }
}
