// Build script: produces ready-to-load packages for Chrome and Firefox plus
// release-ready ZIPs (Firefox zip goes straight to AMO submission).
//   node build.mjs
// Output:
//   dist/chrome/          load unpacked via chrome://extensions
//   dist/firefox/         load temporary via about:debugging
//   dist/vortex-chrome.zip   attach to a GitHub Release / share as needed
//   dist/vortex-firefox.zip  submit to AMO (addons.mozilla.org)
import { mkdirSync, copyFileSync, readFileSync, writeFileSync, cpSync, rmSync } from "node:fs";
import { execFileSync } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";

const root = dirname(fileURLToPath(import.meta.url));
const shared = ["background.js", "content.js", "content.css", "popup.js", "popup.html", "popup.css", "icons"];

const builds = [
  { manifest: "manifest.chrome.json", out: "dist/chrome", zip: "dist/vortex-chrome.zip" },
  { manifest: "manifest.firefox.json", out: "dist/firefox", zip: "dist/vortex-firefox.zip" },
];

function zipDir(outDir, zipPath) {
  rmSync(zipPath, { force: true });
  if (process.platform === "win32") {
    const cmd = `Compress-Archive -Path '${outDir}\\*' -DestinationPath '${zipPath}' -Force`;
    execFileSync("powershell", ["-NoProfile", "-Command", cmd], { stdio: "inherit", cwd: root });
  } else {
    // BSD/macOS tar; on Linux GNU tar ignores -a for zip — install zip if needed.
    execFileSync("tar", ["-a", "-c", "-f", zipPath, "-C", outDir, "."], { stdio: "inherit", cwd: root });
  }
  console.log("zipped " + zipPath);
}

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
  zipDir(b.out, join(root, b.zip));
}

console.log("done — dist/chrome, dist/firefox, dist/vortex-chrome.zip, dist/vortex-firefox.zip");