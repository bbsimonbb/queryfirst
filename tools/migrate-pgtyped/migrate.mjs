#!/usr/bin/env node
// Queryfirst pgtyped migration codemod.
//   node migrate.mjs <project> [--dry-run]
// Splits multi-query .psql/.sql files (one query per file, kebab-cased @name) and
// rewrites TypeScript imports from "x.queries" to "x.query". No dependencies.
import { readdirSync, readFileSync, writeFileSync, existsSync, statSync, unlinkSync } from "node:fs";
import { join, resolve, dirname, basename, extname, relative, sep } from "node:path";
import { pathToFileURL } from "node:url";

const SKIP_DIRS = new Set(["node_modules", ".git", ".next", "dist", "build", ".worktrees", "coverage"]);
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

/** Sorted list of files under root (skipping SKIP_DIRS: dependencies, VCS, build output, worktrees, coverage), optionally filtered by extension. */
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

const SPEC = String.raw`[^"'\n]*?\.queries(?:\.(?:ts|js))?`;
const SRC_RE = new RegExp(
  String.raw`\b(?<kw>import|export)(?<type>\s+type)?(?<ws>\s*)(?:(?<def>[\w$]+)\s*,\s*)?\{(?<raw>[^}]*)\}(?<from>\s*from\s*)(?<q>["'])(?<spec>${SPEC})\k<q>(?<semi>;?)` +
    String.raw`|\b(?<fn>vi|jest)\.mock\(\s*(?<mq>["'])(?<mspec>${SPEC})\k<mq>(?<mtail>\s*(?:,\s*)?)` +
    String.raw`|(?<gq>["'])(?<gspec>${SPEC})\k<gq>`,
  "g",
);

const toQuerySpec = (spec) => spec.replace(/\.queries(?=(?:\.(?:ts|js))?$)/, ".query");

/** Index of the `)` closing a call whose arguments start at `from`, or -1. Skips strings and comments. */
function findCallEnd(text, from) {
  let depth = 0;
  for (let i = from; i < text.length; i++) {
    const c = text[i];
    if (c === '"' || c === "'" || c === "`") {
      for (i++; i < text.length && text[i] !== c; i++) if (text[i] === "\\") i++;
    } else if (c === "/" && text[i + 1] === "/") {
      while (i < text.length && text[i] !== "\n") i++;
    } else if (c === "/" && text[i + 1] === "*") {
      const e = text.indexOf("*/", i + 2);
      if (e < 0) return -1;
      i = e + 1;
    } else if (c === "(" || c === "[" || c === "{") depth++;
    else if (c === ")" || c === "]" || c === "}") {
      if (depth === 0) return c === ")" ? i : -1;
      depth--;
    }
  }
  return -1;
}

const keyRe = (name) => new RegExp(String.raw`(?<![\w$.])${name.replace(/\$/g, "\\$")}\s*(?::|,|\}|\()`);

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
  const targetSpec = (spec, qn) => {
    const ext = (spec.match(/\.queries(\.(?:ts|js))?$/) ?? [])[1] ?? "";
    return `${spec.slice(0, spec.lastIndexOf("/") + 1)}${kebab(qn)}.query${ext}`;
  };
  let out = "";
  let last = 0;
  SRC_RE.lastIndex = 0;
  let m;
  while ((m = SRC_RE.exec(text))) {
    const g = m.groups;
    const whole = m[0];
    let end = m.index + whole.length;
    let rep;
    specifiers++;
    if (g.kw) {
      const split = splitFor(g.spec);
      if (!split) {
        rep = whole.replace(g.spec, toQuerySpec(g.spec));
      } else {
        const members = g.raw.split(",").map((s) => s.trim()).filter(Boolean);
        const groups = new Map();
        const shared = new Set();
        let bad = null;
        for (const mem of members) {
          const name = mem.replace(/^type\s+/, "").split(/\s+as\s+/)[0].trim();
          const qn = split.symbols.get(name);
          if (qn) {
            if (!groups.has(qn)) groups.set(qn, []);
            groups.get(qn).push(mem);
          } else if (split.shared.has(name)) shared.add(mem);
          else bad = name;
        }
        if (bad) {
          manual.push({ specifier: g.spec, reason: `symbol "${bad}" is not exported by the generated module` });
          rep = whole;
        } else {
          // Shared helper types (Json, DateOrString, ...) are only emitted by the split files whose query uses
          // them: send each to the first query group of this import that uses it, else to the split's first query
          // that uses it (a new import line). Unknown usage falls back to the first group (or the first query).
          const fallback = groups.size ? groups.keys().next().value : split.firstName;
          const assigned = new Map();
          for (const mem of shared) {
            const name = mem.replace(/^type\s+/, "").split(/\s+as\s+/)[0].trim();
            const users = split.helperUsers.get(name) ?? [];
            assigned.set(mem, [...groups.keys()].find((k) => users.includes(k)) ?? users[0] ?? fallback);
          }
          for (const qn of new Set(assigned.values())) {
            const mems = new Set([...(groups.get(qn) ?? []), ...[...assigned].filter(([, k]) => k === qn).map(([x]) => x)]);
            groups.set(qn, members.filter((x) => mems.has(x)));
          }
          const multi = g.raw.includes("\n");
          const indent = (g.raw.match(/\n([ \t]+)\S/) ?? [])[1] ?? "  ";
          const comma = /,\s*$/.test(g.raw);
          rep = [...groups]
            .map(([qn, mems], gi) => {
              const body = multi
                ? `{${eol}${mems.map((x, i) => indent + x + (i < mems.length - 1 || comma ? "," : "")).join(eol)}${eol}}`
                : `{${g.raw.startsWith(" ") ? " " : ""}${mems.join(", ")}${/\s$/.test(g.raw) ? " " : ""}}`;
              const def = gi === 0 && g.def ? `${g.def}, ` : "";
              return `${g.kw}${g.type ?? ""}${g.ws}${def}${body}${g.from}${g.q}${targetSpec(g.spec, qn)}${g.q}${g.semi}`;
            })
            .join(eol);
        }
      }
    } else if (g.fn) {
      const split = splitFor(g.mspec);
      if (!split) {
        rep = whole.replace(g.mspec, toQuerySpec(g.mspec));
      } else {
        const head = whole.slice(0, whole.length - g.mtail.length - g.mspec.length - 2);
        const hasFactory = g.mtail.includes(",");
        const close = findCallEnd(text, end);
        const factory = close < 0 ? null : text.slice(end, close);
        let names = null;
        if (close >= 0 && !hasFactory && factory.trim() === "") names = split.names;
        else if (close >= 0 && hasFactory && factory.trim() !== "") {
          names = split.names.filter((n) => split.querySymbols.get(n).some((s) => keyRe(s).test(factory)));
        }
        if (!names || names.length === 0) {
          manual.push({ specifier: g.mspec, reason: `${g.fn}.mock of a split module: no query name found as a key in the factory` });
          rep = whole;
        } else {
          const lineStart = text.lastIndexOf("\n", m.index) + 1;
          const lead = text.slice(lineStart, m.index);
          const pad = /^\s*$/.test(lead) ? lead : "";
          rep = names
            .map((qn) => `${head}${g.mq}${targetSpec(g.mspec, qn)}${g.mq}${g.mtail}${factory})`)
            .join(eol + pad);
          end = close + 1;
        }
      }
    } else if (splitFor(g.gspec)) {
      manual.push({ specifier: g.gspec, reason: "reference to a split module outside an import/export list or mock (require, import(), namespace import)" });
      rep = whole;
    } else rep = `${g.gq}${toQuerySpec(g.gspec)}${g.gq}`;
    out += text.slice(last, m.index) + rep;
    last = end;
    SRC_RE.lastIndex = end;
  }
  out += text.slice(last);
  return { text: out, specifiers, manual };
}

// ---- scope (which query files the codemod may touch) ----

function globToRegExp(glob) {
  const g = glob.replaceAll("\\", "/").replace(/^\.\//, "");
  let re = "";
  let braces = 0;
  for (let i = 0; i < g.length; i++) {
    const c = g[i];
    if (c === "*") {
      if (g[i + 1] === "*") {
        i++;
        if (g[i + 1] === "/") {
          i++;
          re += "(?:.*/)?";
        } else re += ".*";
      } else re += "[^/]*";
    } else if (c === "?") re += "[^/]";
    else if (c === "{") {
      braces++;
      re += "(?:";
    } else if (c === "}" && braces) {
      braces--;
      re += ")";
    } else if (c === "," && braces) re += "|";
    else if (c === "[") {
      const e = g.indexOf("]", i);
      if (e > i) {
        re += g.slice(i, e + 1);
        i = e;
      } else re += "\\[";
    } else re += c.replace(/[.+^$()|\\\]}{]/g, "\\$&");
  }
  return new RegExp(`^${re}$`);
}

const readJson = (f) => JSON.parse(stripJsonc(readFileSync(f, "utf8")));
const asArray = (v) => (v == null ? [] : Array.isArray(v) ? v : [v]);
const DEFAULT_INCLUDE = ["**/*.psql", "**/*.sql"];
const DEFAULT_EXCLUDE = ["**/node_modules/**"];

/**
 * Which files may be split: command line > queryfirst.json > pgtyped-config.json.
 * Returns {source, include, exclude} or null when nothing defines a scope.
 */
export function resolveScope(root, opts = {}) {
  const oi = asArray(opts.include);
  const oe = asArray(opts.exclude);
  let base = null;
  const qf = join(root, "queryfirst.json");
  const pg = join(root, "pgtyped-config.json");
  if (existsSync(qf)) {
    const c = readJson(qf);
    base = { source: "queryfirst.json", include: c.include ?? DEFAULT_INCLUDE, exclude: c.exclude ?? DEFAULT_EXCLUDE };
  } else if (existsSync(pg)) {
    const c = readJson(pg);
    const src = String(c.srcDir ?? ".").replaceAll("\\", "/").replace(/^\.\//, "").replace(/\/+$/, "");
    const prefix = src && src !== "." ? `${src}/` : "";
    const include = [];
    for (const t of asArray(c.transforms)) {
      if (t.mode !== "sql") continue;
      for (const i of asArray(t.include)) include.push(prefix + i.replace(/^\.\//, ""));
    }
    if (include.length) base = { source: "pgtyped-config.json", include, exclude: DEFAULT_EXCLUDE };
  }
  if (!oi.length && !oe.length) return base;
  return {
    source: base ? `command line over ${base.source}` : "command line",
    include: oi.length ? oi : (base?.include ?? DEFAULT_INCLUDE),
    exclude: oe.length ? oe : (base?.exclude ?? DEFAULT_EXCLUDE),
  };
}

export class NoScopeError extends Error {}

/** Compute the migration plan without writing anything. */
export function planMigration(project, opts = {}) {
  const root = resolve(project);
  const scope = resolveScope(root, opts);
  if (!scope) {
    throw new NoScopeError(
      "no scope: found neither queryfirst.json nor pgtyped-config.json with a sql transform; pass --include <glob> (and optionally --exclude <glob>)",
    );
  }
  const inc = scope.include.map(globToRegExp);
  const exc = scope.exclude.map(globToRegExp);
  const inScope = (file) => {
    const rel = relative(root, file).split(sep).join("/");
    return inc.some((r) => r.test(rel)) && !exc.some((r) => r.test(rel));
  };
  const aliases = loadAliases(root);
  const splits = [];
  const skipped = [];
  const conflicts = [];
  const splitMap = new Map();
  const claimed = new Map();

  for (const file of walk(root, [".psql", ".sql"])) {
    if (!inScope(file)) continue;
    const text = readFileSync(file, "utf8");
    const qs = parseQueries(text);
    if (qs.length < 2) continue;
    const stem = file.slice(0, file.length - extname(file).length);
    const generated = `${stem}.queries.ts`;
    if (!existsSync(generated)) {
      skipped.push({ file, reason: "no sibling .queries.ts; not a pgtyped-managed file" });
      continue;
    }
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
    const querySymbols = new Map();
    for (const t of targets) {
      const syms = symbolsOf(t.name);
      querySymbols.set(t.name, syms);
      for (const s of syms) symbols.set(s, t.name);
    }
    const genText = readFileSync(generated, "utf8");
    const exported = [
      ...genText.matchAll(/^export\s+(?:declare\s+)?(?:type|interface|const|enum|class|function|let|var)\s+([\w$]+)/gm),
    ].map((x) => x[1]);
    const shared = new Set(exported.filter((n) => !symbols.has(n)));
    // Which queries use each shared helper, in file order: pgtyped writes one block per query, starting
    // at its `/** '<Pascal>' parameters type */` comment.
    const helperUsers = new Map([...shared].map((h) => [h, []]));
    const starts = [...genText.matchAll(/^\/\*\* '[\w$]+' parameters type \*\/\r?$/gm)].map((x) => x.index);
    for (const [i, at] of starts.entries()) {
      const block = genText.slice(at, starts[i + 1] ?? genText.length);
      const qn = (/export const ([\w$]+) = new PreparedQuery/.exec(block) ?? [])[1];
      if (!qn || !names.has(qn)) continue;
      for (const h of shared) if (new RegExp(`\\b${h.replace(/\$/g, "\\$")}\\b`).test(block)) helperUsers.get(h).push(qn);
    }
    const key = norm(stem);
    if (splitMap.has(key)) {
      conflicts.push(`${file}: collides with ${splitMap.get(key).file} (same name, different extension; both generate ${basename(generated)})`);
    }
    splitMap.set(key, { file, symbols, querySymbols, shared, helperUsers, names: targets.map((t) => t.name), firstName: targets[0].name });
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
    const rewrittenCount = r.specifiers - r.manual.length;
    if (rewrittenCount > 0 || r.manual.length) {
      rewrites.push({ file, specifiers: rewrittenCount, text: r.text, changed: r.text !== text });
    }
  }
  return { root, scope, splits, skipped, rewrites, manual, conflicts };
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
  log(`scope: ${plan.scope.source} (include ${plan.scope.include.join(", ")}; exclude ${plan.scope.exclude.join(", ")})`);
  for (const s of plan.splits) {
    log(`split ${rel(s.file)} ->`);
    for (const t of s.targets) log(`    ${rel(t.path)}`);
  }
  for (const s of plan.skipped) log(`skipped ${rel(s.file)}: ${s.reason}`);
  if (dryRun) for (const r of plan.rewrites) if (r.specifiers) log(`rewrite imports in ${rel(r.file)} (${r.specifiers})`);
  for (const m of plan.manual) log(`MANUAL REVIEW ${rel(m.file)}: "${m.specifier}" - ${m.reason}`);
  const imports = plan.rewrites.reduce((n, r) => n + r.specifiers, 0);
  const touched = plan.rewrites.filter((r) => r.changed).length;
  log("");
  log(`Queryfirst pgtyped migration${dryRun ? " (dry run, nothing written)" : ""}`);
  log(`  files split: ${plan.splits.length} (${plan.splits.reduce((n, s) => n + s.targets.length, 0)} query files)`);
  log(`  imports rewritten: ${imports}`);
  log(`  TypeScript files touched: ${touched}`);
  log(`  skipped (unmanaged): ${plan.skipped.length}`);
  log(`  needing manual review: ${plan.manual.length}`);
  if (!dryRun) log("Next: run `qf generate`, review, then delete the old *.queries.ts files.");
}

export function main(argv) {
  const usage = "usage: node migrate.mjs <project> [--dry-run] [--include <glob>]... [--exclude <glob>]...";
  let dryRun = false;
  const positional = [];
  const opts = { include: [], exclude: [] };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    const eq = a.match(/^--(include|exclude)=(.+)$/);
    if (a === "--dry-run") dryRun = true;
    else if (eq) opts[eq[1]].push(eq[2]);
    else if (a === "--include" || a === "--exclude") {
      if (i + 1 >= argv.length) {
        console.error(`${a} needs a glob\n${usage}`);
        return 2;
      }
      opts[a.slice(2)].push(argv[++i]);
    } else if (a.startsWith("-")) {
      console.error(`unknown option ${a}\n${usage}`);
      return 2;
    } else positional.push(a);
  }
  if (positional.length !== 1) {
    console.error(usage);
    return 2;
  }
  if (!existsSync(positional[0]) || !statSync(positional[0]).isDirectory()) {
    console.error(`not a directory: ${positional[0]}\n${usage}`);
    return 2;
  }
  let plan;
  try {
    plan = planMigration(positional[0], opts);
  } catch (e) {
    if (e instanceof NoScopeError) {
      console.error(`${e.message}\n${usage}`);
      return 2;
    }
    throw e;
  }
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
