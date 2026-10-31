import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync, chmodSync } from "node:fs";
import { tmpdir } from "node:os";
import { delimiter, dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const shim = join(dirname(fileURLToPath(import.meta.url)), "..", "bin", "qf.js");
const pkgName = `queryfirst-${process.platform}-${process.arch}`;
const exe = process.platform === "win32" ? "qf.exe" : "qf";

// The fake "binary" is a copy of node itself; NODE_OPTIONS preloads a script that
// prints argv and exits 3 before node tries to run the args as a script.
function makeFakePlatform(root) {
  const nm = join(root, "node_modules");
  const dir = join(nm, pkgName);
  mkdirSync(join(dir, "bin"), { recursive: true });
  writeFileSync(join(dir, "package.json"), JSON.stringify({ name: pkgName, version: "0.0.0" }));
  copyFileSync(process.execPath, join(dir, "bin", exe));
  chmodSync(join(dir, "bin", exe), 0o755);
  const preload = join(root, "fake.cjs");
  writeFileSync(preload, `console.log("ARGV:" + JSON.stringify(process.argv.slice(2).concat(process.argv.slice(1,2).map(p => require("path").basename(p)))));process.exit(3);`);
  return { nm, preload };
}

function run(args, env) {
  return spawnSync(process.execPath, [shim, ...args], { encoding: "utf8", env: { ...process.env, ...env } });
}

test("passes args and exit code through to the platform binary", () => {
  const root = mkdtempSync(join(tmpdir(), "qf-shim-"));
  try {
    const { nm, preload } = makeFakePlatform(root);
    const r = run(["generate", "x"], {
      NODE_PATH: nm,
      NODE_OPTIONS: `--require ${JSON.stringify(preload.split("\\").join("/"))}`,
    });
    assert.equal(r.status, 3, r.stderr);
    assert.match(r.stdout, /ARGV:/);
    assert.match(r.stdout, /generate/);
    assert.match(r.stdout, /"x"|x/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("missing platform package prints a clear message and exits 2", () => {
  const empty = mkdtempSync(join(tmpdir(), "qf-shim-empty-"));
  try {
    const r = run(["--version"], { NODE_PATH: empty });
    assert.equal(r.status, 2);
    assert.ok(r.stderr.includes(pkgName), r.stderr);
    assert.ok(r.stderr.includes("Queryfirst"), r.stderr);
  } finally {
    rmSync(empty, { recursive: true, force: true });
  }
});
