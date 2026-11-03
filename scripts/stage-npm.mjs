#!/usr/bin/env node
// Stages the npm packages for publishing.
//
//   node scripts/stage-npm.mjs --dry-run
//   node scripts/stage-npm.mjs [--out target/npm] [--binaries <dir>]
//
// The version comes from the workspace Cargo.toml and is stamped into every package.json
// (and into the optionalDependencies of `queryfirst`). <dir> holds built binaries named
// qf-<os>-<cpu> (qf-win32-x64.exe on Windows); missing ones are skipped with a warning.
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync, chmodSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const args = process.argv.slice(2);
const dryRun = args.includes("--dry-run");
const opt = (name, dflt) => {
  const i = args.indexOf(name);
  return i >= 0 ? args[i + 1] : dflt;
};
const out = resolve(root, opt("--out", "target/npm"));
const binDir = opt("--binaries", null) && resolve(opt("--binaries"));

const cargo = readFileSync(join(root, "Cargo.toml"), "utf8");
const version = /\[workspace\.package\][^[]*?\nversion\s*=\s*"([^"]+)"/.exec(cargo)?.[1];
if (!version) throw new Error("could not read [workspace.package] version from Cargo.toml");

const platforms = ["win32-x64", "linux-x64", "linux-arm64", "darwin-x64", "darwin-arm64"];
const readJson = (p) => JSON.parse(readFileSync(p, "utf8"));
const write = (p, j) => writeFileSync(p, JSON.stringify(j, null, 2) + "\n");

const main = readJson(join(root, "npm/queryfirst/package.json"));
main.version = version;
for (const p of platforms) main.optionalDependencies[`@queryfirst/cli-${p}`] = version;

const log = (s) => console.log(s);
log(`version ${version}${dryRun ? " (dry run, nothing written)" : ""}`);
log(`out: ${out}`);
log(`queryfirst@${version}`);
log("  bin/qf.js, README.md, queryfirst.schema.json, qf-meta.v1.schema.json");
for (const p of platforms) log(`  optionalDependency @queryfirst/cli-${p}@${version}`);

const exeName = (p) => (p.startsWith("win32") ? "qf.exe" : "qf");
const srcBinary = (p) => binDir && join(binDir, p.startsWith("win32") ? `qf-${p}.exe` : `qf-${p}`);
for (const p of platforms) {
  const src = srcBinary(p);
  const state = src && existsSync(src) ? `binary ${src}` : "no binary (skipped)";
  log(`@queryfirst/cli-${p}@${version} (staged as queryfirst-${p}/)\n  bin/${exeName(p)}  <- ${state}`);
}
log(`@queryfirst/runtime@${version}\n  published from npm/queryfirst-runtime (workflow stamps the version; prepack builds dist)`);

if (!dryRun) {
  rmSync(out, { recursive: true, force: true });
  const mainOut = join(out, "queryfirst");
  mkdirSync(join(mainOut, "bin"), { recursive: true });
  cpSync(join(root, "npm/queryfirst/bin/qf.js"), join(mainOut, "bin/qf.js"));
  cpSync(join(root, "npm/queryfirst/README.md"), join(mainOut, "README.md"));
  cpSync(join(root, "crates/queryfirst/schema/queryfirst.schema.json"), join(mainOut, "queryfirst.schema.json"));
  cpSync(join(root, "crates/qf-meta/schema/qf-meta.v1.schema.json"), join(mainOut, "qf-meta.v1.schema.json"));
  write(join(mainOut, "package.json"), main);

  for (const p of platforms) {
    const dir = join(out, `queryfirst-${p}`);
    mkdirSync(join(dir, "bin"), { recursive: true });
    const pj = readJson(join(root, "npm/platforms", p, "package.json"));
    pj.version = version;
    write(join(dir, "package.json"), pj);
    const src = srcBinary(p);
    if (src && existsSync(src)) {
      const dest = join(dir, "bin", exeName(p));
      cpSync(src, dest);
      chmodSync(dest, 0o755);
    } else {
      console.warn(`warning: no binary for ${p}`);
    }
  }
}
