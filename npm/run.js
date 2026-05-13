#!/usr/bin/env node
"use strict";

const { spawnSync } = require("child_process");
const path = require("path");
const fs = require("fs");

const bin = path.join(__dirname, "bin", process.platform === "win32" ? "aizo.exe" : "aizo");

if (!fs.existsSync(bin)) {
  console.error(
    "[aizo] Binary not found. Try reinstalling: npm install aizo-node\n" +
    "Or install via Cargo: cargo install aizo"
  );
  process.exit(1);
}

const result = spawnSync(bin, process.argv.slice(2), { stdio: "inherit" });
process.exit(result.status ?? 1);
