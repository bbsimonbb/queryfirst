#!/usr/bin/env node
"use strict";
// Thin launcher: finds the platform-specific package holding the native `qf` binary and runs it.
const { spawnSync } = require("node:child_process");
const path = require("node:path");

const pkg = `queryfirst-${process.platform}-${process.arch}`;
const exe = process.platform === "win32" ? "qf.exe" : "qf";

let pkgJson;
try {
  pkgJson = require.resolve(`${pkg}/package.json`);
} catch {
  process.stderr.write(
    `Queryfirst: the platform package "${pkg}" is not installed.\n` +
      `It is an optional dependency of "queryfirst" and is installed automatically on supported platforms ` +
      `(win32-x64, linux-x64, linux-arm64, darwin-x64, darwin-arm64).\n` +
      `Try reinstalling without --no-optional / --omit=optional, or install "${pkg}" explicitly.\n`,
  );
  process.exit(2);
}

const binary = path.join(path.dirname(pkgJson), "bin", exe);
const result = spawnSync(binary, process.argv.slice(2), { stdio: "inherit" });

if (result.error) {
  process.stderr.write(`Queryfirst: failed to run ${binary}: ${result.error.message}\n`);
  process.exit(2);
}
if (result.signal) {
  process.kill(process.pid, result.signal);
  // If the signal did not terminate us, fall back to a conventional failure code.
  process.exit(1);
}
process.exit(result.status ?? 1);
