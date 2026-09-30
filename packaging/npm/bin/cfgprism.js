#!/usr/bin/env node
// npx cfgprism support: downloads the matching release binary on first use.
// The published npm tarball only carries this script (binaries come from
// GitHub releases, like cargo-dist's own npm installer).
"use strict";
const { execFileSync } = require("child_process");
const fs = require("fs");
const os = require("os");
const path = require("path");

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

function main() {
  const dir = path.join(__dirname, "vendor");
  const exe = path.join(dir, process.platform === "win32" ? "cfgprism.exe" : "cfgprism");
  if (!fs.existsSync(exe)) {
    console.error(`cfgprism: binary not installed yet. Run \`node install.js\` or set it up via the package README.`);
    console.error(`Expected at ${exe} (release v${VERSION}, triple ${triple()}, repo ${REPO}).`);
    process.exit(1);
  }
  execFileSync(exe, process.argv.slice(2), { stdio: "inherit" });
}

main();
