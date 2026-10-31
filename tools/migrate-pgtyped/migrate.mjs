#!/usr/bin/env node
// Queryfirst pgtyped migration codemod.
//   node migrate.mjs <project> [--dry-run]
// Splits multi-query .psql/.sql files (one query per file, kebab-cased @name) and
// rewrites TypeScript imports from "x.queries" to "x.query". No dependencies.
import { readdirSync, readFileSync, writeFileSync, existsSync, statSync, unlinkSync } from "node:fs";
import { join, resolve, dirname, basename, extname, relative, sep } from "node:path";
import { pathToFileURL } from "node:url";

const SKIP_DIRS = new Set(["node_modules", ".git", ".next"]);
const HEADER_RE = /\/\*\s*@name\s+([A-Za-z_]\w*)/g;

export function kebab(name) {
  return name
    .replace(/([a-z0-9])([A-Z])/g, "$1-$2")
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1-$2")
    .replace(/_/g, "-")
    .toLowerCase();
}

export function pascal(name) {
  return name
    .split(/[_-]+/)
    .filter(Boolean)
    .map((p) => p[0].toUpperCase() + p.slice(1))
    .join("");
}

/** Sorted list of files under root (skipping node_modules/.git/.next), optionally filtered by extension. */
export function walk(root, exts) {
  const out = [];
  const rec = (dir) => {
    for (const e of readdirSync(dir, { withFileTypes: true }).sort((a, b) => (a.name < b.name ? -1 : 1))) {
      const p = join(dir, e.name);
      if (e.isDirectory()) {
        if (!SKIP_DIRS.has(e.name)) rec(p);
      } else if (!exts || exts.includes(extname(e.name))) out.push(p);
    }
  };
  rec(root);
  return out;
}

const norm = (p) => resolve(p).replaceAll("\\", "/").toLowerCase();

function stripJsonc(text) {
  let out = "";
  for (let i = 0; i < text.length; i++) {
    const c = text[i];
    if (c === '"') {
      let j = i + 1;
      while (j < text.length && text[j] !== '"') j += text[j] === "\\" ? 2 : 1;
      out += text.slice(i, j + 1);
      i = j;
    } else if (c === "/" && text[i + 1] === "/") {
      while (i < text.length && text[i] !== "\n") i++;
      out += "\n";
    } else if (c === "/" && text[i + 1] === "*") {
      i = text.indexOf("*/", i + 2);
      i = i < 0 ? text.length : i + 1;
    } else out += c;
  }
  return out.replace(/,(\s*[}\]])/g, "$1");
}

/** tsconfig `paths` as [{prefix, suffix, targets:[abs dir/prefix patterns]}]; falls back to "@/*" -> "./*". */
function loadAliases(root) {
  let paths = { "@/*": ["./*"] };
  let baseDir = root;
  const f = join(root, "tsconfig.json");
  if (existsSync(f)) {
    try {
      const co = JSON.parse(stripJsonc(readFileSync(f, "utf8"))).compilerOptions ?? {};
      if (co.baseUrl) baseDir = resolve(root, co.baseUrl);
      if (co.paths) paths = co.paths;
    } catch {
      /* keep fallback */
    }
  }
  return Object.entries(paths).map(([pat, targets]) => {
    const star = pat.indexOf("*");
    return {
      prefix: star < 0 ? pat : pat.slice(0, star),
      exact: star < 0,
      targets: targets.map((t) => resolve(baseDir, t.replace("*", "\u0000"))),
    };
  });
}

function candidates(spec, fromFile, aliases) {
  if (spec.startsWith(".")) return [resolve(dirname(fromFile), spec)];
  const out = [];
  for (const a of aliases) {
    if (a.exact ? spec === a.prefix : spec.startsWith(a.prefix)) {
      const rest = a.exact ? "" : spec.slice(a.prefix.length);
      for (const t of a.targets) out.push(t.replace("\u0000", rest));
    }
  }
  return out;
}

/** Parse one source file's text for query headers. */
function parseQueries(text) {
  const heads = [...text.matchAll(HEADER_RE)];
  return heads.map((m, i) => ({
    name: m[1],
    start: m.index,
    end: i + 1 < heads.length ? heads[i + 1].index : text.length,
  }));
}

function symbolsOf(name) {
  const P = pascal(name);
  return [name, `I${P}Params`, `I${P}Result`, `I${P}Query`];
}

const STMT_OR_STRING = new RegExp(
  String.raw`\b(import|export)(\s+type)?(\s*)\{([^}]*)\}(\s*from\s*)(["'])([^"'\n]+?\.queries(?:\.(?:ts|js))?)\6(;?)` +
    "|" +
    String.raw`(["'])([^"'\n]*\.queries(?:\.(?:ts|js))?)\9`,
  "g",
);

const toQuerySpec = (spec) => spec.replace(/\.queries(?=(?:\.(?:ts|js))?$)/, ".query");

/** Rewrite one TS file's text. Returns {text, specifiers, manual:[{specifier, reason}]}. */
function rewriteSource(text, file, splitMap, aliases) {
  const eol = text.includes("\r\n") ? "\r\n" : "\n";
  let specifiers = 0;
  const manual = [];
  const splitFor = (spec) => {
    const base = spec.replace(/\.queries(?:\.(?:ts|js))?$/, "");
    for (const c of candidates(base, file, aliases)) {
      const s = splitMap.get(norm(c));
      if (s) return s;
    }
    return null;
  };
  const out = text.replace(STMT_OR_STRING, (...m) => {
    if (m[1]) {
      const [whole, kw, type = "", ws, raw, from, q, spec, semi] = m;
      specifiers++;
      const split = splitFor(spec);
      if (!split) return `${kw}${type}${ws}{${raw}}${from}${q}${toQuerySpec(spec)}${q}${semi}`;
      const members = raw.split(",").map((s) => s.trim()).filter(Boolean);
      const groups = new Map();
      for (const mem of members) {
        const name = mem.replace(/^type\s+/, "").split(/\s+as\s+/)[0].trim();
        const q2 = split.symbols.get(name);
        if (!q2) {
          manual.push({ specifier: spec, reason: `symbol "${name}" not exported by any split query` });
          return whole;
        }
        if (!groups.has(q2)) groups.set(q2, []);
        groups.get(q2).push(mem);
      }
      const multi = raw.includes("\n");
      const indent = (raw.match(/\n([ \t]+)\S/) ?? [])[1] ?? "  ";
      const comma = /,\s*$/.test(raw);
      const prefix = spec.slice(0, spec.lastIndexOf("/") + 1);
      const ext = (spec.match(/\.queries(\.(?:ts|js))?$/) ?? [])[1] ?? "";
      return [...groups]
        .map(([qn, mems]) => {
          const body = multi
            ? `{${eol}${mems.map((x, i) => indent + x + (i < mems.length - 1 || comma ? "," : "")).join(eol)}${eol}}`
            : `{${raw.startsWith(" ") ? " " : ""}${mems.join(", ")}${/\s$/.test(raw) ? " " : ""}}`;
          return `${kw}${type}${ws}${body}${from}${q}${prefix}${kebab(qn)}.query${ext}${q}${semi}`;
        })
        .join(eol);
    }
    const [whole, , , , , , , , , q, spec] = m;
    specifiers++;
    if (splitFor(spec)) {
      manual.push({ specifier: spec, reason: "reference to a split module outside an import/export list (vi.mock, require, import(), default/namespace import)" });
      return whole;
    }
    return `${q}${toQuerySpec(spec)}${q}`;
  });
  return { text: out, specifiers, manual };
}

/** Compute the migration plan without writing anything. */
export function planMigration(project) {
  const root = resolve(project);
  const aliases = loadAliases(root);
  const splits = [];
  const conflicts = [];
  const splitMap = new Map();
  const claimed = new Map();

  for (const file of walk(root, [".psql", ".sql"])) {
    const text = readFileSync(file, "utf8");
    const qs = parseQueries(text);
    if (qs.length < 2) continue;
    const eol = text.includes("\r\n") ? "\r\n" : "\n";
    const names = new Set();
    const preamble = text.slice(0, qs[0].start).trim() ? text.slice(0, qs[0].start) : "";
    const targets = [];
    for (const [i, q] of qs.entries()) {
      if (names.has(q.name)) conflicts.push(`${file}: duplicate @name ${q.name}`);
      names.add(q.name);
      const target = join(dirname(file), `${kebab(q.name)}.psql`);
      const body = (i === 0 ? preamble : "") + text.slice(q.start, q.end);
      targets.push({ name: q.name, path: target, content: body.replace(/\s+$/, "") + eol });
      const key = norm(target);
      if (claimed.has(key) && claimed.get(key) !== file) conflicts.push(`${target}: also produced by ${claimed.get(key)}`);
      else if (targets.filter((t) => norm(t.path) === key).length > 1) conflicts.push(`${target}: two queries in ${file} map to this name`);
      claimed.set(key, file);
      if (norm(target) !== norm(file) && existsSync(target)) conflicts.push(`${target}: already exists (splitting ${file})`);
    }
    const symbols = new Map();
    for (const t of targets) for (const s of symbolsOf(t.name)) symbols.set(s, t.name);
    splitMap.set(norm(file.slice(0, file.length - extname(file).length)), { symbols });
    splits.push({ file, targets });
  }

  const rewrites = [];
  const manual = [];
  for (const file of walk(root, [".ts", ".tsx"])) {
    if (/\.queries\.ts$/.test(file)) continue;
    const text = readFileSync(file, "utf8");
    if (!text.includes(".queries")) continue;
    const r = rewriteSource(text, file, splitMap, aliases);
    for (const mn of r.manual) manual.push({ file, ...mn });
    const changed = r.text !== text;
    const rewrittenCount = r.specifiers - r.manual.length;
    if (rewrittenCount > 0 || r.manual.length) {
      rewrites.push({ file, specifiers: rewrittenCount, text: r.text, changed });
    }
  }
  return { root, splits, rewrites, manual, conflicts };
}

export function applyPlan(plan) {
  for (const s of plan.splits) {
    for (const t of s.targets) if (norm(t.path) !== norm(s.file)) writeFileSync(t.path, t.content);
    const same = s.targets.find((t) => norm(t.path) === norm(s.file));
    if (same) writeFileSync(s.file, same.content);
    else unlinkSync(s.file);
  }
  for (const r of plan.rewrites) if (r.changed) writeFileSync(r.file, r.text);
}

function summarize(plan, dryRun, log) {
  const rel = (p) => relative(plan.root, p).split(sep).join("/");
  for (const s of plan.splits) {
    log(`split ${rel(s.file)} ->`);
    for (const t of s.targets) log(`    ${rel(t.path)}`);
  }
  if (dryRun) for (const r of plan.rewrites) if (r.specifiers) log(`rewrite imports in ${rel(r.file)} (${r.specifiers})`);
  for (const m of plan.manual) log(`MANUAL REVIEW ${rel(m.file)}: "${m.specifier}" - ${m.reason}`);
  const imports = plan.rewrites.reduce((n, r) => n + r.specifiers, 0);
  const touched = plan.rewrites.filter((r) => r.changed).length;
  log("");
  log(`Queryfirst pgtyped migration${dryRun ? " (dry run, nothing written)" : ""}`);
  log(`  files split: ${plan.splits.length} (${plan.splits.reduce((n, s) => n + s.targets.length, 0)} query files)`);
  log(`  imports rewritten: ${imports}`);
  log(`  TypeScript files touched: ${touched}`);
  log(`  needing manual review: ${plan.manual.length}`);
  if (!dryRun) log("Next: run `qf generate`, review, then delete the old *.queries.ts files.");
}

export function main(argv) {
  const args = argv.slice();
  const dryRun = args.includes("--dry-run");
  const rest = args.filter((a) => a !== "--dry-run");
  const unknown = rest.filter((a) => a.startsWith("-"));
  const usage = "usage: node migrate.mjs <project> [--dry-run]";
  if (unknown.length || rest.length !== 1) {
    console.error(unknown.length ? `unknown option ${unknown[0]}\n${usage}` : usage);
    return 2;
  }
  if (!existsSync(rest[0]) || !statSync(rest[0]).isDirectory()) {
    console.error(`not a directory: ${rest[0]}\n${usage}`);
    return 2;
  }
  const plan = planMigration(rest[0]);
  if (plan.conflicts.length) {
    console.error("Queryfirst migration aborted; nothing was written. Conflicts:");
    for (const c of plan.conflicts) console.error(`  ${c}`);
    return 1;
  }
  summarize(plan, dryRun, (l) => console.log(l));
  if (!dryRun) applyPlan(plan);
  return 0;
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  process.exitCode = main(process.argv.slice(2));
}
