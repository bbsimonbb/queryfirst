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

// The fake platform package holds a real child process "binary" that prints a marker and
// its argv, then exits 3. Nothing is preloaded into the shim itself.
//  - POSIX: a node-shebang script named `qf`.
//  - Windows: spawn() cannot run .cmd files without a shell, so `qf.exe` is a copy of node.exe;
//    it receives the shim's first arg ("generate") as its script, which resolves relative to the
//    cwd, so the test runs the shim from a dir containing an extensionless script `generate`.
const childSource = `console.log("FAKE-QF-CHILD " + JSON.stringify(process.argv.slice(2)));process.exit(3);
`;

function makeFakePlatform(root) {
  const nm = join(root, "node_modules");
  const dir = join(nm, pkgName);
  mkdirSync(join(dir, "bin"), { recursive: true });
  writeFileSync(join(dir, "package.json"), JSON.stringify({ name: pkgName, version: "0.0.0" }));
  if (process.platform === "win32") {
    copyFileSync(process.execPath, join(dir, "bin", exe));
    writeFileSync(join(root, "generate"), childSource);
  } else {
    writeFileSync(join(dir, "bin", exe), "#!/usr/bin/env node\n" + childSource);
    chmodSync(join(dir, "bin", exe), 0o755);
  }
  return nm;
}

function run(args, env, cwd) {
  return spawnSync(process.execPath, [shim, ...args], { encoding: "utf8", cwd, env: { ...process.env, ...env } });
}

test("spawns the platform binary as a child, passing args and exit code through", () => {
  const root = mkdtempSync(join(tmpdir(), "qf-shim-"));
  try {
    const nm = makeFakePlatform(root);
    const r = run(["generate", "x"], { NODE_PATH: nm }, root);
    assert.ok(r.stdout.includes("FAKE-QF-CHILD"), "child marker missing: " + r.stdout + r.stderr);
    assert.ok(r.stdout.includes('["x"]') || r.stdout.includes('["generate","x"]'), r.stdout);
    assert.equal(r.status, 3, r.stderr);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("missing platform package prints a clear message and exits 2", () => {
  const empty = mkdtempSync(join(tmpdir(), "qf-shim-empty-"));
  try {
    const r = run(["--version"], { NODE_PATH: empty }, empty);
    assert.equal(r.status, 2);
    assert.ok(r.stderr.includes(pkgName), r.stderr);
    assert.ok(r.stderr.includes("Queryfirst"), r.stderr);
  } finally {
    rmSync(empty, { recursive: true, force: true });
  }
});
