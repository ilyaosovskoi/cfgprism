#!/usr/bin/env node
// Downloads the platform cfgprism binary from GitHub releases into vendor/.
"use strict";
const fs = require("fs");
const https = require("https");
const os = require("os");
const path = require("path");
const { execSync } = require("child_process");

const VERSION = process.env.CFGPRISM_VERSION || require("./package.json").version;
const REPO = "ilyaosovskoi/cfgprism";

function triple() {
  const plat = os.platform();
  const arch = os.arch();
  if (plat === "linux" && arch === "x64") return "x86_64-unknown-linux-gnu";
  if (plat === "darwin" && arch === "arm64") return "aarch64-apple-darwin";
  if (plat === "darwin" && arch === "x64") return "x86_64-apple-darwin";
  if (plat === "win32" && arch === "x64") return "x86_64-pc-windows-msvc";
  throw new Error(`cfgprism: unsupported platform ${plat}-${arch}`);
}

function fetch(url, dest) {
  return new Promise((resolve, reject) => {
    https
      .get(url, { headers: { "User-Agent": "cfgprism-npm" } }, (res) => {
        if (res.statusCode >= 300 && res.statusCode < 400 && res.headers.location) {
          return resolve(fetch(res.headers.location, dest));
        }
        if (res.statusCode !== 200) {
          return reject(new Error(`HTTP ${res.statusCode} for ${url}`));
        }
        const out = fs.createWriteStream(dest);
        res.pipe(out);
        out.on("finish", () => resolve(dest));
      })
      .on("error", reject);
  });
}

(async () => {
  const t = triple();
  const ext = t.includes("windows") ? "zip" : "tar.gz";
  const url = `https://github.com/${REPO}/releases/download/v${VERSION}/cfgprism-${t}.${ext}`;
  const dir = path.join(__dirname, "vendor");
  fs.mkdirSync(dir, { recursive: true });
  const archive = path.join(dir, `cfgprism.${ext}`);
  console.error(`cfgprism: downloading ${url}`);
  await fetch(url, archive);
  if (ext === "zip") {
    execSync(`powershell -Command "Expand-Archive -Path '${archive}' -DestinationPath '${dir}' -Force"`, { stdio: "inherit" });
  } else {
    execSync(`tar -xzf '${archive}' -C '${dir}'`, { stdio: "inherit" });
  }
  const exe = path.join(dir, t.includes("windows") ? "cfgprism.exe" : "cfgprism");
  if (!t.includes("windows")) fs.chmodSync(exe, 0o755);
  console.error(`cfgprism: installed ${exe}`);
})().catch((e) => {
  console.error(`cfgprism: install failed: ${e.message}`);
  process.exit(1);
});
