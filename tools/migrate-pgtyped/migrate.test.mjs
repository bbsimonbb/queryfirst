import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, readdirSync, existsSync, statSync } from "node:fs";
import { join, dirname, relative } from "node:path";
import { tmpdir } from "node:os";
import { fileURLToPath } from "node:url";
import { planMigration, kebab, pascal, walk } from "./migrate.mjs";

const SCRIPT = join(dirname(fileURLToPath(import.meta.url)), "migrate.mjs");

function put(root, rel, text) {
  const p = join(root, rel);
  mkdirSync(dirname(p), { recursive: true });
  writeFileSync(p, text);
}

function fixture({ eol = "\n" } = {}) {
  const root = mkdtempSync(join(tmpdir(), "qf-mig-"));
  const j = (s) => s.split("\n").join(eol);
  put(root, "tsconfig.json", `{ // comment\n "compilerOptions": { "paths": { "@/*": ["./*"] }, },\n}`);
  put(root, "src/q/user.psql", j(
    "/* @name getUser */\nSELECT * FROM u WHERE id = :id;\n\n" +
    "/* @name listUsers */\nSELECT * FROM u;\n\n" +
    "/* @name deleteUser */\nDELETE FROM u WHERE id = :id;\n"));
  put(root, "src/q/user.queries.ts", "export const getUser = 1;\n");
  put(root, "src/q/single.psql", j("/* @name single */\nSELECT 1;\n"));
  put(root, "src/a.ts", j(
    'import { getUser, IGetUserResult } from "./q/user.queries";\n' +
    'import { single } from "./q/single.queries";\n'));
  put(root, "src/b.ts", j(
    "import type {\n  IListUsersParams as P,\n  IDeleteUserQuery,\n  IGetUserParams,\n} from '@/src/q/user.queries'\n" +
    'export { listUsers } from "./q/user.queries";\n' +
    'import { single as s } from "./q/single.queries.ts";\n' +
    'vi.mock("./q/single.queries", () => ({}));\n' +
    'vi.mock("./q/user.queries", () => ({}));\n'));
  put(root, "node_modules/x/c.ts", 'import { a } from "./z.queries";\n');
  return root;
}

function tree(root) {
  const out = {};
  for (const f of walk(root)) out[relative(root, f).replaceAll("\\", "/")] = readFileSync(f, "utf8");
  return out;
}
const run = (...args) => spawnSync(process.execPath, [SCRIPT, ...args], { encoding: "utf8" });

test("kebab and pascal follow pgtyped naming", () => {
  assert.equal(kebab("getAccount"), "get-account");
  assert.equal(kebab("getURLValue"), "get-url-value");
  assert.equal(pascal("getAccount"), "GetAccount");
  assert.equal(pascal("get_account"), "GetAccount");
});

test("splits multi-query file and rewrites imports (LF)", () => {
  const root = fixture();
  const r = run(root);
  assert.equal(r.status, 0, r.stderr);
  const t = tree(root);
  assert.ok(!("src/q/user.psql" in t));
  assert.equal(t["src/q/get-user.psql"], "/* @name getUser */\nSELECT * FROM u WHERE id = :id;\n");
  assert.equal(t["src/q/list-users.psql"], "/* @name listUsers */\nSELECT * FROM u;\n");
  assert.ok(t["src/q/delete-user.psql"].startsWith("/* @name deleteUser */"));
  assert.ok("src/q/single.psql" in t);
  assert.equal(t["src/a.ts"],
    'import { getUser, IGetUserResult } from "./q/get-user.query";\n' +
    'import { single } from "./q/single.query";\n');
  assert.equal(t["src/b.ts"],
    "import type {\n  IListUsersParams as P,\n} from '@/src/q/list-users.query'\n" +
    "import type {\n  IDeleteUserQuery,\n} from '@/src/q/delete-user.query'\n" +
    "import type {\n  IGetUserParams,\n} from '@/src/q/get-user.query'\n" +
    'export { listUsers } from "./q/list-users.query";\n' +
    'import { single as s } from "./q/single.query.ts";\n' +
    'vi.mock("./q/single.query", () => ({}));\n' +
    'vi.mock("./q/user.queries", () => ({}));\n');
  assert.equal(readFileSync(join(root, "node_modules/x/c.ts"), "utf8"), 'import { a } from "./z.queries";\n');
  assert.ok(t["src/q/user.queries.ts"]);
  assert.match(r.stdout, /files split: 1/i);
});

test("one import pulling from several split queries becomes several lines (single-line, CRLF kept)", () => {
  const root = fixture({ eol: "\r\n" });
  put(root, "src/c.ts", 'import { getUser, listUsers as l, type IDeleteUserResult } from "./q/user.queries";\r\nconst x = 1;\r\n');
  assert.equal(run(root).status, 0);
  const t = tree(root);
  assert.equal(t["src/c.ts"],
    'import { getUser } from "./q/get-user.query";\r\n' +
    'import { listUsers as l } from "./q/list-users.query";\r\n' +
    'import { type IDeleteUserResult } from "./q/delete-user.query";\r\nconst x = 1;\r\n');
  assert.ok(!t["src/a.ts"].includes("\n") || t["src/a.ts"].split("\n").every((l) => l === "" || l.endsWith("\r")));
  assert.ok(t["src/q/get-user.psql"].includes("\r\n"));
  assert.ok(!/[^\r]\n/.test(t["src/q/get-user.psql"]));
});

test("--dry-run writes nothing and prints the plan", () => {
  const root = fixture();
  const before = tree(root);
  const r = run(root, "--dry-run");
  assert.equal(r.status, 0, r.stderr);
  assert.deepEqual(tree(root), before);
  assert.match(r.stdout, /get-user\.psql/);
  assert.match(r.stdout, /dry run/i);
});

test("conflict aborts with exit 1 and writes nothing", () => {
  const root = fixture();
  put(root, "src/q/list-users.psql", "existing");
  const before = tree(root);
  const r = run(root);
  assert.equal(r.status, 1);
  assert.deepEqual(tree(root), before);
  assert.match(r.stderr, /list-users\.psql/);
});

test("usage errors exit 2", () => {
  assert.equal(run().status, 2);
  assert.equal(run("--bogus", "x").status, 2);
  assert.equal(run(join(tmpdir(), "qf-does-not-exist-xyz")).status, 2);
});

test("a split target equal to the original name is allowed", () => {
  const root = mkdtempSync(join(tmpdir(), "qf-mig-"));
  put(root, "q/get-a.psql", "/* @name getA */\nSELECT 1;\n/* @name getB */\nSELECT 2;\n");
  assert.equal(run(root).status, 0);
  const t = tree(root);
  assert.equal(t["q/get-a.psql"], "/* @name getA */\nSELECT 1;\n");
  assert.equal(t["q/get-b.psql"], "/* @name getB */\nSELECT 2;\n");
});

test("corpus smoke (read-only, QF_PGTYPED_CORPUS)", { skip: !process.env.QF_PGTYPED_CORPUS }, () => {
  const root = process.env.QF_PGTYPED_CORPUS;
  const plan = planMigration(root);
  assert.equal(plan.splits.length, 26);
  assert.deepEqual(plan.conflicts, []);
  let specifiers = 0;
  for (const f of walk(root, [".ts", ".tsx"])) {
    if (/\.queries\.ts$/.test(f)) continue;
    const m = readFileSync(f, "utf8").match(/["'][^"'\n]*\.queries(?:\.(?:ts|js))?["']/g);
    specifiers += m ? m.length : 0;
  }
  const rewritten = plan.rewrites.reduce((n, r) => n + r.specifiers, 0);
  const manual = plan.manual.length;
  console.log(`corpus: specifiers=${specifiers} rewritten=${rewritten} manual=${manual} splits=${plan.splits.length}`);
  assert.ok(specifiers > 1000);
  assert.equal(rewritten + manual, specifiers);
});
