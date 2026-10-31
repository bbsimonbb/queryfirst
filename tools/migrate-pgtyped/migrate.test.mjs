import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, unlinkSync } from "node:fs";
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
  put(root, "pgtyped-config.json", '{"srcDir":"./","transforms":[{"mode":"sql","include":"**/*.*sql"}]}');
  put(root, "src/q/user.psql", j(
    "/* @name getUser */\nSELECT * FROM u WHERE id = :id;\n\n" +
    "/* @name listUsers */\nSELECT * FROM u;\n\n" +
    "/* @name deleteUser */\nDELETE FROM u WHERE id = :id;\n"));
  put(root, "src/q/user.queries.ts",
    "export type Json = null | boolean;\nexport type DateOrString = Date | string;\nexport interface IGetUserParams {}\nexport const getUser = 1;\n");
  put(root, "src/q/single.psql", j("/* @name single */\nSELECT 1;\n"));
  put(root, "ops/index.psql", "/* @name a */\nSELECT 1;\n/* @name b */\nSELECT 2;\n");
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
const rels = (root, list) => list.map((s) => relative(root, s.file).replaceAll("\\", "/"));

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
  assert.ok("ops/index.psql" in t, "unmanaged multi-query file must survive");
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
  assert.match(r.stdout, /scope: pgtyped-config\.json/);
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
  put(root, "pgtyped-config.json", '{"transforms":[{"mode":"sql","include":"**/*.psql"}]}');
  put(root, "q/get-a.queries.ts", "export const getA = 1;\n");
  put(root, "q/get-a.psql", "/* @name getA */\nSELECT 1;\n/* @name getB */\nSELECT 2;\n");
  assert.equal(run(root).status, 0);
  const t = tree(root);
  assert.equal(t["q/get-a.psql"], "/* @name getA */\nSELECT 1;\n");
  assert.equal(t["q/get-b.psql"], "/* @name getB */\nSELECT 2;\n");
});

test("scope: no config and no override refuses with exit 2; --include overrides", () => {
  const root = fixture();
  unlinkSync(join(root, "pgtyped-config.json"));
  const before = tree(root);
  const r = run(root, "--dry-run");
  assert.equal(r.status, 2);
  assert.match(r.stderr, /--include/);
  assert.equal(run(root).status, 2);
  assert.deepEqual(tree(root), before);
  const ok = run(root, "--dry-run", "--include", "src/**/*.psql");
  assert.equal(ok.status, 0, ok.stderr);
  assert.match(ok.stdout, /scope: command line/);
});

test("scope: guard skips multi-query files without a sibling .queries.ts, and says so", () => {
  const root = fixture();
  const plan = planMigration(root);
  assert.deepEqual(rels(root, plan.splits), ["src/q/user.psql"]);
  assert.deepEqual(rels(root, plan.skipped), ["ops/index.psql"]);
  assert.match(run(root, "--dry-run").stdout, /skipped ops\/index\.psql/);
});

test("scope: pgtyped srcDir, then queryfirst.json include/exclude", () => {
  const root = fixture();
  put(root, "ops/index.queries.ts", "export const a = 1;\n");
  put(root, "pgtyped-config.json", '{"srcDir":"./src/","transforms":[{"mode":"sql","include":"**/*.*sql"}]}');
  let plan = planMigration(root);
  assert.deepEqual(rels(root, plan.splits), ["src/q/user.psql"]);
  put(root, "queryfirst.json", '{"exclude":["src/**"]}');
  plan = planMigration(root);
  assert.equal(plan.scope.source, "queryfirst.json");
  assert.deepEqual(rels(root, plan.splits), ["ops/index.psql"]);
});

test("shared helper types follow the first query group, in any position; default import kept", () => {
  const root = fixture();
  put(root, "src/s1.ts",
    'import { Json, DateOrString, listUsers, getUser } from "./q/user.queries";\n' +
    'import type { Json as J } from "./q/user.queries";\n' +
    'import Def, { IListUsersResult, Json as K } from "./q/user.queries";\n' +
    'import { listUsers as l2, DateOrString as D } from "./q/user.queries";\n' +
    'import { Nope, getUser as g } from "./q/user.queries";\n');
  const r = run(root);
  assert.equal(r.status, 0, r.stderr);
  assert.equal(readFileSync(join(root, "src/s1.ts"), "utf8"),
    'import { Json, DateOrString, listUsers } from "./q/list-users.query";\n' +
    'import { getUser } from "./q/get-user.query";\n' +
    'import type { Json as J } from "./q/get-user.query";\n' +
    'import Def, { IListUsersResult, Json as K } from "./q/list-users.query";\n' +
    'import { listUsers as l2, DateOrString as D } from "./q/list-users.query";\n' +
    'import { Nope, getUser as g } from "./q/user.queries";\n');
  assert.match(r.stdout, /Nope/);
});

test("shared type listed after the query symbol of a later group", () => {
  const root = fixture();
  put(root, "src/s2.ts", 'import { deleteUser, getUser, Json } from "./q/user.queries";\n');
  assert.equal(run(root).status, 0);
  assert.equal(readFileSync(join(root, "src/s2.ts"), "utf8"),
    'import { deleteUser, Json } from "./q/delete-user.query";\n' +
    'import { getUser } from "./q/get-user.query";\n');
});

test("shared helper types go to a query whose generated block uses them (found in simplyfirst acceptance)", () => {
  const root = fixture();
  // Real pgtyped layout: helpers in the head, one block per query; only the queries that use a
  // helper re-export it once split, so Json must follow listUsers and DateOrString deleteUser.
  put(root, "src/q/user.queries.ts", [
    '/** Types generated for queries found in "src/q/user.psql" */',
    "export type Json = null | boolean;",
    "",
    "export type DateOrString = Date | string;",
    "",
    "/** 'GetUser' parameters type */",
    "export interface IGetUserParams { id?: number | null | void; }",
    "export const getUser = new PreparedQuery<IGetUserParams,IGetUserResult>(getUserIR);",
    "",
    "/** 'ListUsers' parameters type */",
    "export type IListUsersParams = void;",
    "export interface IListUsersResult { data: Json | null; }",
    "export const listUsers = new PreparedQuery<IListUsersParams,IListUsersResult>(listUsersIR);",
    "",
    "/** 'DeleteUser' parameters type */",
    "export interface IDeleteUserParams { at?: DateOrString | null | void; }",
    "export const deleteUser = new PreparedQuery<IDeleteUserParams,IDeleteUserResult>(deleteUserIR);",
    "",
  ].join("\n"));
  put(root, "src/s3.ts", [
    'import { Json, getUser, listUsers } from "./q/user.queries";',
    'import { getUser as g, DateOrString } from "./q/user.queries";',
    'import type { Json as J } from "./q/user.queries";',
    "",
  ].join("\n"));
  const r = run(root);
  assert.equal(r.status, 0, r.stderr);
  assert.equal(readFileSync(join(root, "src/s3.ts"), "utf8"), [
    'import { getUser } from "./q/get-user.query";',
    'import { Json, listUsers } from "./q/list-users.query";',
    'import { getUser as g } from "./q/get-user.query";',
    'import { DateOrString } from "./q/delete-user.query";',
    'import type { Json as J } from "./q/list-users.query";',
    "",
  ].join("\n"));
});

test("vi.mock of a split module becomes one mock per target module", () => {
  const root = fixture({ eol: "\r\n" });
  put(root, "src/m.test.ts",
    'vi.mock("./q/user.queries", () => ({\r\n  getUser: { run: m.a },\r\n  deleteUser: { run: m.getUser },\r\n}))\r\n' +
    'vi.mock("./q/user.queries")\r\n' +
    'vi.mock("./q/user.queries", () => ({ unknownThing: 1 }))\r\n' +
    'vi.mock("./q/single.queries", () => ({ single: 1 }))\r\n');
  const r = run(root);
  assert.equal(r.status, 0, r.stderr);
  assert.equal(readFileSync(join(root, "src/m.test.ts"), "utf8"),
    'vi.mock("./q/get-user.query", () => ({\r\n  getUser: { run: m.a },\r\n  deleteUser: { run: m.getUser },\r\n}))\r\n' +
    'vi.mock("./q/delete-user.query", () => ({\r\n  getUser: { run: m.a },\r\n  deleteUser: { run: m.getUser },\r\n}))\r\n' +
    'vi.mock("./q/get-user.query")\r\nvi.mock("./q/list-users.query")\r\nvi.mock("./q/delete-user.query")\r\n' +
    'vi.mock("./q/user.queries", () => ({ unknownThing: 1 }))\r\n' +
    'vi.mock("./q/single.query", () => ({ single: 1 }))\r\n');
  assert.match(r.stdout, /MANUAL REVIEW src\/m\.test\.ts/);
});

test("x.psql and x.sql side by side are a conflict; nothing written", () => {
  const root = fixture();
  put(root, "src/q/user.sql", "/* @name other1 */\nSELECT 1;\n/* @name other2 */\nSELECT 2;\n");
  const before = tree(root);
  const r = run(root);
  assert.equal(r.status, 1);
  assert.deepEqual(tree(root), before);
  assert.match(r.stderr, /collides/);
});

test("corpus smoke (read-only, QF_PGTYPED_CORPUS)", { skip: !process.env.QF_PGTYPED_CORPUS }, () => {
  const root = process.env.QF_PGTYPED_CORPUS;
  const plan = planMigration(root);
  assert.equal(plan.splits.length, 25);
  assert.ok(!plan.splits.some((s) => /sql-migration/.test(s.file.replaceAll("\\", "/"))), "sql-migration must not be planned");
  assert.deepEqual(plan.conflicts, []);
  let specifiers = 0;
  for (const f of walk(root, [".ts", ".tsx"])) {
    if (/\.queries\.ts$/.test(f)) continue;
    const m = readFileSync(f, "utf8").match(/["'][^"'\n]*\.queries(?:\.(?:ts|js))?["']/g);
    specifiers += m ? m.length : 0;
  }
  const rewritten = plan.rewrites.reduce((n, r) => n + r.specifiers, 0);
  const manual = plan.manual.length;
  console.log(`corpus: scope=${plan.scope.source} specifiers=${specifiers} rewritten=${rewritten} manual=${manual} splits=${plan.splits.length} skipped=${plan.skipped.length}`);
  for (const m of plan.manual) console.log(`  manual: ${m.file} ${m.specifier} ${m.reason}`);
  assert.ok(specifiers > 1000);
  assert.equal(rewritten + manual, specifiers);
});
