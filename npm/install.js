#!/usr/bin/env node
// Downloads the correct platform binary from GitHub releases on `npm install`.
"use strict";

const https = require("https");
const fs = require("fs");
const path = require("path");
const { execSync } = require("child_process");
const zlib = require("zlib");

const pkg = require("./package.json");
const VERSION = pkg.version;
const REPO = "mmmarcinho/aizo";
const BIN_DIR = path.join(__dirname, "bin");
const BIN_PATH = path.join(BIN_DIR, process.platform === "win32" ? "aizo.exe" : "aizo");

// ── Platform → release asset name ────────────────────────────────────────────

const TARGETS = {
  "linux-x64":   "aizo-x86_64-unknown-linux-gnu.tar.gz",
  "linux-arm64": "aizo-aarch64-unknown-linux-gnu.tar.gz",
  "darwin-x64":  "aizo-x86_64-apple-darwin.tar.gz",
  "darwin-arm64":"aizo-aarch64-apple-darwin.tar.gz",
  "win32-x64":   "aizo-x86_64-pc-windows-msvc.zip",
};

function platformKey() {
  return `${process.platform}-${process.arch}`;
}

function releaseUrl(asset) {
  return `https://github.com/${REPO}/releases/download/v${VERSION}/${asset}`;
}

// ── HTTP helpers ──────────────────────────────────────────────────────────────

function get(url, cb) {
  https.get(url, (res) => {
    if (res.statusCode === 301 || res.statusCode === 302) {
      return get(res.headers.location, cb);
    }
    if (res.statusCode !== 200) {
      cb(new Error(`HTTP ${res.statusCode} for ${url}`));
      return;
    }
    const chunks = [];
    res.on("data", (c) => chunks.push(c));
    res.on("end", () => cb(null, Buffer.concat(chunks)));
    res.on("error", cb);
  }).on("error", cb);
}

// ── tar.gz extraction (pure Node, no child_process) ──────────────────────────

function extractTarGz(buf, destDir) {
  // Decompress gzip
  const tar = zlib.gunzipSync(buf);

  // Minimal tar parser — finds the first entry whose name ends with /aizo
  let offset = 0;
  while (offset + 512 <= tar.length) {
    const header = tar.slice(offset, offset + 512);
    const name = header.slice(0, 100).toString("utf8").replace(/\0/g, "").trim();
    const sizeOctal = header.slice(124, 136).toString("utf8").replace(/\0/g, "").trim();
    const size = parseInt(sizeOctal, 8) || 0;
    offset += 512;

    if (name === "" || size === 0) { offset += Math.ceil(size / 512) * 512; continue; }

    const isAizoBin = /(?:^|[/\\])aizo(?:\.exe)?$/.test(name);
    if (isAizoBin) {
      const data = tar.slice(offset, offset + size);
      fs.mkdirSync(destDir, { recursive: true });
      const dest = path.join(destDir, process.platform === "win32" ? "aizo.exe" : "aizo");
      fs.writeFileSync(dest, data, { mode: 0o755 });
      return dest;
    }
    offset += Math.ceil(size / 512) * 512;
  }
  throw new Error("aizo binary not found in archive");
}

// ── zip extraction (Windows) — delegates to PowerShell ───────────────────────

function extractZip(buf, destDir) {
  const tmp = path.join(destDir, "_aizo.zip");
  fs.mkdirSync(destDir, { recursive: true });
  fs.writeFileSync(tmp, buf);
  execSync(
    `powershell -NoProfile -Command "Expand-Archive -Force -Path '${tmp}' -DestinationPath '${destDir}'"`,
    { stdio: "inherit" }
  );
  fs.unlinkSync(tmp);
  // Find the .exe
  const exe = fs.readdirSync(destDir).find((f) => f.endsWith(".exe"));
  if (!exe) throw new Error("aizo.exe not found after zip extraction");
  const dest = path.join(destDir, "aizo.exe");
  fs.renameSync(path.join(destDir, exe), dest);
  return dest;
}

// ── Main ──────────────────────────────────────────────────────────────────────

if (fs.existsSync(BIN_PATH)) {
  process.exit(0); // already installed
}

const key = platformKey();
const asset = TARGETS[key];

if (!asset) {
  console.error(
    `[aizo] Unsupported platform: ${key}.\n` +
    `Install via cargo instead: cargo install aizo`
  );
  process.exit(0); // non-fatal — maybe user builds from source
}

const url = releaseUrl(asset);
console.log(`[aizo] Downloading ${asset} …`);

get(url, (err, buf) => {
  if (err) {
    console.error(`[aizo] Download failed: ${err.message}`);
    console.error(`[aizo] You can also install via: cargo install aizo`);
    process.exit(0); // non-fatal
  }

  try {
    const dest = asset.endsWith(".zip")
      ? extractZip(buf, BIN_DIR)
      : extractTarGz(buf, BIN_DIR);
    console.log(`[aizo] Installed to ${dest}`);
  } catch (e) {
    console.error(`[aizo] Extraction failed: ${e.message}`);
    process.exit(1);
  }
});
