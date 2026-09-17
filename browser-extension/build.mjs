// Build script: produces ready-to-load packages for Chrome and Firefox.
//   node build.mjs
// Output:
//   dist/chrome/   — load unpacked via chrome://extensions
//   dist/firefox/  — load temporary via about:debugging (or zip for AMO)
import { mkdirSync, copyFileSync, readFileSync, writeFileSync, cpSync, rmSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = dirname(fileURLToPath(import.meta.url));
const shared = ["background.js", "content.js", "content.css", "popup.js", "popup.html", "popup.css", "icons"];

const builds = [
  { manifest: "manifest.chrome.json", out: "dist/chrome" },
  { manifest: "manifest.firefox.json", out: "dist/firefox" },
];

for (const b of builds) {
  const outDir = join(root, b.out);
  rmSync(outDir, { recursive: true, force: true });
  mkdirSync(outDir, { recursive: true });
  for (const f of shared) {
    const src = join(root, f);
    if (f.endsWith(".css") || f.endsWith(".js") || f.endsWith(".html")) copyFileSync(src, join(outDir, f));
    else cpSync(src, join(outDir, f), { recursive: true });
  }
  writeFileSync(join(outDir, "manifest.json"), readFileSync(join(root, b.manifest)));
  console.log("built " + b.out);
}