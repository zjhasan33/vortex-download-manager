// Generates a 1024x1024 Vortex app icon (gradient V on dark rounded square)
import { deflateSync } from "node:zlib";
import { writeFileSync, mkdirSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const S = 1024;
const R = 210;
const px = new Uint8Array(S * S * 3);

// helpers
const clamp01 = (v) => Math.max(0, Math.min(1, v));
function hex(c) {
  return [
    parseInt(c.slice(1, 3), 16),
    parseInt(c.slice(3, 5), 16),
    parseInt(c.slice(5, 7), 16),
  ];
}
const lerp = (a, b, t) => a + (b - a) * t;
function mix(c1, c2, t) {
  return [lerp(c1[0], c2[0], t), lerp(c1[1], c2[1], t), lerp(c1[2], c2[2], t)];
}

const C_TOP = hex("#0b1120");
const C_BOTTOM = hex("#131c38");
const C_V_TOP = hex("#22d3ee");
const C_V_MID = hex("#818cf8");
const C_V_BOT = hex("#c084fc");

function distToSeg(px, py, x1, y1, x2, y2) {
  const dx = x2 - x1, dy = y2 - y1;
  const l2 = dx * dx + dy * dy;
  let t = l2 === 0 ? 0 : ((px - x1) * dx + (py - y1) * dy) / l2;
  t = clamp01(t);
  const cx = x1 + t * dx, cy = y1 + t * dy;
  return Math.hypot(px - cx, py - cy);
}

// between two segments, return min distance and param t for gradient
function vDist(px, py) {
  const segs = [
    [330, 330, 512, 665],
    [694, 330, 512, 665],
  ];
  let best = { d: 1e9, t: 0 };
  for (const [x1, y1, x2, y2] of segs) {
    const dx = x2 - x1, dy = y2 - y1;
    const l2 = dx * dx + dy * dy;
    let t = l2 === 0 ? 0 : ((px - x1) * dx + (py - y1) * dy) / l2;
    t = clamp01(t);
    const cx = x1 + t * dx, cy = y1 + t * dy;
    const d = Math.hypot(px - cx, py - cy);
    if (d < best.d) best = { d, t: t * 0.45 + (y1 > 500 ? 0.45 : 0) };
  }
  return best;
}

const insideRound = (x, y) => {
  const rx = Math.min(x, S - 1 - x);
  const ry = Math.min(y, S - 1 - y);
  if (rx >= R || ry >= R) return true;
  return Math.hypot(rx - R, ry - R) <= R;
};

let row = 0;
for (let y = 0; y < S; y++) {
  row++;
  for (let x = 0; x < S; x++) {
    const o = (y * S + x) * 3;
    if (!insideRound(x, y)) {
      px[o] = 0; px[o + 1] = 0; px[o + 2] = 0;
      continue;
    }
    // background gradient
    const t = y / S;
    let bg = mix(C_TOP, C_BOTTOM, t);
    // top glow
    const gl = Math.exp(-(y / 300) - (Math.abs(x - S / 2) / 420));
    bg = mix(bg, hex("#272f5e"), gl * 0.8);
    // bottom subtle glow
    const gb = Math.exp(-(Math.abs(y - S) / 380) - (Math.abs(x - S / 2) / 400));
    bg = mix(bg, [34, 211, 238], gb * 0.12);

    let r = bg[0], g = bg[1], b = bg[2];

    const { d, t: vt } = vDist(x, y);
    const thickness = 36;
    if (d < thickness) {
      const c = mix(C_V_TOP, mix(C_V_MID, C_V_BOT, clamp01((vt - 0.3) / 0.7)), clamp01((y - 330) / 340));
      const a = 1;
      r = lerp(r, c[0], a);
      g = lerp(g, c[1], a);
      b = lerp(b, c[2], a);
    } else if (d < thickness + 46) {
      const k = clamp01((d - thickness) / 46);
      const glowA = Math.exp(-k * 3.2) * 0.5;
      const c = mix(C_V_TOP, C_V_BOT, clamp01((y - 330) / 340));
      r = lerp(r, c[0], glowA);
      g = lerp(g, c[1], glowA);
      b = lerp(b, c[2], glowA);
    }

    // speed lines near bottom
    const ly = [762, 788];
    for (const sy of ly) {
      const d2 = Math.abs(y - sy);
      const segHalf = sy === 762 ? 130 : 90;
      if (d2 < 8 && Math.abs(x - S / 2) < segHalf) {
        const a = (1 - d2 / 8) * 0.85;
        r = lerp(r, 167, a); g = lerp(g, 180, a); b = lerp(b, 255, a);
      }
    }

    px[o] = Math.round(r);
    px[o + 1] = Math.round(g);
    px[o + 2] = Math.round(b);
  }
}

// ---- PNG encode ----
function crc32(buf) {
  let c, table = crc32.table;
  if (!table) {
    table = crc32.table = new Int32Array(256);
    for (let n = 0; n < 256; n++) {
      c = n;
      for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
      table[n] = c;
    }
  }
  c = -1;
  for (let i = 0; i < buf.length; i++) c = table[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ -1) >>> 0;
}
function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const t = Buffer.from(type, "ascii");
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([t, data])));
  return Buffer.concat([len, t, data, crc]);
}
const sig = Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]);
const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(S, 0);
ihdr.writeUInt32BE(S, 4);
ihdr[8] = 8; // bit depth
ihdr[9] = 2; // color type RGB
const raw = Buffer.alloc(S * (S * 3 + 1));
for (let y = 0; y < S; y++) {
  raw[y * (S * 3 + 1)] = 0;
  px.subarray(y * S * 3, (y + 1) * S * 3).forEach((v, i) => {
    raw[y * (S * 3 + 1) + 1 + i] = v;
  });
}
const idat = deflateSync(raw, { level: 9 });
const png = Buffer.concat([
  sig,
  chunk("IHDR", ihdr),
  chunk("IDAT", idat),
  chunk("IEND", Buffer.alloc(0)),
]);

const outDir = join(dirname(fileURLToPath(import.meta.url)), "..", "src-tauri", "icons");
mkdirSync(outDir, { recursive: true });
const out = join(outDir, "app-icon.png");
writeFileSync(out, png);
console.log("icon written:", out);