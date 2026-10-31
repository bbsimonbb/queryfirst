# Queryfirst — design

Date: 2026-10-30
Status: approved 2026-10-30

- **Branding:** "Queryfirst", always with a small f. npm package: `queryfirst`.
- **Repository:** a new repo. On this machine, and anywhere else the name clashes, the folder is `queryfirst-ai`. This repo (the original C#/Visual Studio Queryfirst) stays maintained as a Visual Studio extension, independently of the new tool. This spec gets copied into the new repo when it's created (*my call*).

## 1. Intent

Queryfirst lets an application use real `.sql` files as first-class source code. A command-line tool validates every query against the development database and generates a strongly typed wrapper in the host language that represents the real shapes the query takes and returns.

The original Queryfirst (this repo) proved the idea as a Visual Studio extension. The extension half was a decade-long maintenance burden; the core idea was golden. Simplyfirst now uses pgtyped for the same job, and pgtyped is painful: a single-file mode that never works, a watch that regenerates every query, line-ending churn, and ~4 minutes per run over ~940 queries.

The new Queryfirst is a lightweight, cross-platform CLI, **designed for agents first**. The principle: validating queries against the database during development matters as much to agents as it does to people. An agent edits a `.sql` file, runs one command, and gets precise, schema-aware feedback in under a second.

Simplyfirst is the first user. The first deliverable is a drop-in replacement for pgtyped in `simplyfirst-app`.

## 2. Goals, non-goals, anti-goals

### Goals
- One-shot CLI, fast enough to run after every edit. A single query: well under a second. A full run over simplyfirst-app's ~940 queries: target under 10 seconds against a local dev DB.
- **Deep regeneration on every run.** Every run re-describes every query in scope against the live DB, so a DB refactor surfaces every broken query at once.
- **Deterministic output.** Same input → byte-identical output. A file is only rewritten if its content would change. A clean run leaves `git status` clean.
- A committed **meta file** per query that records its inputs and outputs. This is the contract between the core, the generators, `test`, and agents.
- **Test** (`qf test`): re-describe every query against any database and compare with the committed meta files, to catch failed or skipped migrations. The main use is in place, in the deployed environment, against the live app's own datasource (§9a). No test framework is needed.
- **Schema inspection** for agents (`qf schema`).
- Easy to add host languages. Generators read meta JSON and know nothing about databases.
- Drop-in replacement for pgtyped's TypeScript output in simplyfirst-app.
- Postgres first, SQL Server supported.
- Cross-platform portability is the primary technical criterion.

### Non-goals (for now)
- Watch mode as the default. If it exists at all, it is opt-in.
- Editor/IDE integration, language server, auto-connecting SQL editors.
- Design-time blocks (the old Queryfirst `-- designTime` section).
- Multiple queries per file.
- A test-framework self-test (the original generated xUnit tests). `qf test` is a CLI step.
- A database in CI. Testing happens in place after deployment (§9a).
- Executing queries during generate or test. We only describe.

### Anti-goals (failure even if it technically works)
- Churn: a run that touches files that didn't meaningfully change.
- Slowness that makes people or agents stop running it.
- Silent wrongness: passing a broken query, claiming a column is non-null when it can be null, or silently losing precision.
- Adding a host language requires changing the Rust core.
- Additive schema changes (a new column) break deployed apps or fail `qf test`.

## 2a. Milestones (*my call*)

- **Milestone 1 (what the first plan covers):** Postgres core, nullability and parameter analyzers, meta files, built-in TypeScript generator (pgtyped drop-in plus `@type`, `@returns`, optional `@name`), `generate`, `describe`, `check`, `test`, `schema`, `init --from-pgtyped`, `bundle`, npm distribution, and the simplyfirst-app migration.
- **Later milestones, each designed and planned on its own:** SQL Server provider **with both C# and TypeScript generators**, the external generator protocol, phase 2 conversion (§13), and the bookmarks (§17).
- Since SQL Server will need TypeScript output too, the milestone 1 TypeScript generator keeps type generation (params and results types from the meta file) separate from the runtime binding (`PreparedQuery` and IR for `@pgtyped/runtime`). That way a SQL Server runtime binding can be added later without rewriting the generator. Milestone 1 builds only the pgtyped binding.
- Milestone 1 doesn't build later-milestone analyzers. The meta schema is versioned so it can grow (CHECK constraints, JSON shapes) without breaking anything.

## 3. Key workflows

1. **Agent writes or edits a query.** It runs `qf schema <tables>` to learn the real columns, then edits `foo.psql` and runs `qf generate app/foo.psql`. On error, it gets `file:line:col` plus a schema-aware message (with `--json`, a structured version). It fixes the query and reruns. It never edits generated files; `qf check` flags hand edits. A broken query also leaves its state in the meta file and a single compile error in the wrapper (§10a).
2. **DB refactor.** A developer changes the schema and runs `qf generate` with no arguments. Every query is re-described. The output is a full list of broken queries, plus a git diff limited to the queries whose shapes changed.
3. **Build / CI (no database).** `qf check` fails if a `.sql` file changed since its meta was generated (source hash ≠ meta), a wrapper was edited by hand, a wrapper doesn't match its meta, or a meta file records a broken query. It never connects to a DB. Developers and agents run `qf generate` against the dev DB.
4. **Deploy (in place).** Simplyfirst deploys blue/green with a health check. Once green is up, `qf test` runs in green's environment against the datasource the live app is configured with, and checks the deployed meta files using the compatibility rules (§9). A failure fails the health check, so traffic doesn't switch to a build whose queries don't match the DB (§9a).
5. **Migration from pgtyped.** A one-off: config conversion, then splitting the 26 multi-query files, then a first generate. Generated output is equivalent to pgtyped's (§11).

## 4. Architecture

```
 .psql/.sql ──► core (Rust) ──► .qf.json meta ──► generator(s) ──► host code
                  │  ▲
                  ▼  │ describe (no execution)
               dev database
 test:    committed .qf.json ──► describe against target DB ──► diff + compatibility verdict
```

- **Core (Rust, single static binary `qf`).** Parses query files, describes them against the DB, analyzes nullability, writes meta files, runs generators, and centralises all file writing (determinism, line endings, only-if-changed). Also provides `check`, `test`, `schema`, `init` and `bundle`.
- **Meta file.** One per query, committed, next to the source: `foo.psql` → `foo.qf.json` (§7).
- **Generators.** Each one is a function from meta files to output files. Generators never touch the DB and never write files themselves; they return `{path, content}` and the core writes.
  - The TypeScript generator ships built in to the core, as Rust templates.
  - External generators use a protoc-style protocol: an executable that gets a JSON batch of meta files on stdin and returns a JSON list of files on stdout. Any language can implement one. The protocol is versioned.
- **Runtime libraries (host side).** Phase 1: the generated TS still uses `@pgtyped/runtime`, plus a small Queryfirst runtime package for `@returns` (§6.4). Its name is the builder's call: `@queryfirst/runtime` if the npm scope can be claimed, an unscoped name otherwise. The same applies wherever `@queryfirst/runtime` appears below. Phase 2 adds the client-safe conversion library (§13).

Why Rust: portability. One CI matrix produces static binaries for every platform, and npm distribution of per-platform binaries is well established (Biome, SWC, oxc). The libpg_query bindings (`pg_query`, maintained by pganalyze) and `tokio-postgres` (which reports parameter types and the table OID and column number behind each result column) are first-rate. We considered C# (Native AOT needs a runner per OS; libpg_query would need native interop) and Go (cgo for libpg_query removes the easy cross-compilation) and rejected both. Known risk: the Rust SQL Server driver (Tiberius) is community-maintained. We only use it to call `sp_describe_*` procedures, which limits the exposure.

The pipeline is a fixed linear sequence per query (read → describe → analyze → meta → generate), parallel across queries. No reactive or graph runtime (Octopus was considered and isn't a fit for a run-once tool).

## 5. Query files

- Extensions: `.psql` and `.sql` (configurable globs). **One query per file**, enforced.
- Leading block comment holds annotations. Everything else is the SQL statement, which ends with an optional trailing `;`.
- **pgtyped syntax is accepted unchanged**, so existing files work:
  - `/* @name fooBar */`: optional (§6.1).
  - Parameters: `:param` (nullable), `:param!` (required). New: `:param?` (explicitly nullable; overrides inference).
  - Expansions: `@param ids -> (...)`, `@param user -> (name, age)`, `@param users -> ((name, age)...)`, with the same semantics as pgtyped. Unused in simplyfirst-app today, but kept for compatibility.
  - Column nullability overrides by alias: `AS "total!"` (non-null), `AS "name?"` (nullable). The suffix is stripped from the output name.
- Other comments at the top that aren't annotations become the query's **description**.
- The `:param` lexer must match pgtyped's: it must ignore `::casts`, string literals, quoted identifiers, dollar-quoted strings, and comments. The generated IR has to be byte-compatible with `@pgtyped/runtime` (§11).

## 6. Annotations beyond the DB

The query author knows things the DB can't report. Each annotation goes into the meta file, so every generator can use it.

### 6.1 `@name` becomes optional
If `@name` is absent, the name comes from the file name, camel-cased: `get-accounts.psql` → `getAccounts`. If both exist and disagree, `@name` wins and the tool reports a warning. The migration codemod (§12) names split files so their exports stay identical.

### 6.2 `@type`: type overrides for columns and parameters
```sql
/* @type theme        -> Themes from "@/types/user/theme"
   @type vatCountries -> VatCountries[] from "@/types/vat"
   @type variants     -> IVariant[] from "@/types/catalogue" */
```
- Applies to a result column or a parameter (resolved by name; parameters take the form `@type :param -> …`).
- Covers both string → app enum or union (367 hand corrections in simplyfirst-app) and Json → app type (42 hand corrections).
- It's the author's declaration and isn't checked against the data. The meta file keeps the DB type as well, and `qf test` still checks the DB type.
- Nullability composes on top: if the column is nullable, the TypeScript type becomes `Themes | null`.
- Meta representation: `{ "name": "Themes", "module": "@/types/user/theme", "array": true|false }`. The TS generator emits `import type { … } from "…"` and aliases names when they collide.

### 6.3 Nullability overrides
See the alias suffixes and `:p!` / `:p?` in §5.

### 6.4 `@returns one | maybe | many | none`
- Absent: `run()` returns an array, as in pgtyped (drop-in).
- `many`: array (explicit).
- `maybe`: returns `Row | null`. Throws if there is more than one row.
- `one`: returns `Row`. Throws if there are 0 or more than 1 rows.
- `none`: returns `void`.
- Adding `@returns` to a query changes the return type of `run()`. Call sites that then fail to compile are fixed then; adding the annotation is the opt-in. Implemented through `@queryfirst/runtime`, which wraps pgtyped's `PreparedQuery`.
- *Builder's call:* whether to infer `maybe` for `WHERE pk = :p` and report it as a suggestion. The tool must never apply an inferred result count without the author's annotation.

## 7. Meta file (`foo.qf.json`)

The contract. Its schema is versioned and documented, and it's stable enough for third parties to write generators against. Contents (the exact schema is the builder's to define):

- `version`, `source` (relative path), `name`, `description`
- `sqlHash` (hash of the normalised source file), `statement` (the SQL as sent to the DB, with parameter positions). The statement is embedded so `qf test` can run from meta files alone.
- `provider` (`postgres` | `sqlserver`)
- `params[]`: name, position(s), DB type (with typmod), `required` + `requiredReason` (`annotation` | `inferred:insert-notnull` | `inferred:where-eq-notnull` | `default-nullable`), expansion kind, `@type` override
- `columns[]` (ordered): name, DB type (with typmod: precision, scale, length), `nullable` + `nullableReason` (`proven-notnull` | `outer-join` | `expression` | `analyzer-gave-up` | `override`), source table/column if known, `@type` override
- `returns` (annotation or absent)
- `generated`: for each generated file, its path and a hash of its content (used to detect hand edits)

Formatting is deterministic: stable key order, 2-space indent, LF, trailing newline.

## 8. Describing queries

**Nothing is ever executed.** Describe only.

### 8.1 Postgres
1. Prepare the statement over the extended protocol (Parse/Describe). This gives the parameter type OIDs and the result columns, with type OID, typmod, table OID and column attnum. If the server can't infer a parameter's type, the error is reported, with a hint to cast in SQL (`:p::int`).
2. Parse the statement with libpg_query to get the syntax tree.
3. **Nullability analyzer.** The rule is nullable unless proven otherwise. A column is non-null only if:
   - it is a direct reference to a `NOT NULL` table column (looked up in the catalog) and that relation isn't on the nullable side of any outer join, at any nesting level, including subqueries and CTEs; or
   - it is an expression on a small allowlist: `COUNT(*)`/`COUNT(x)`, literals, `COALESCE(…, <non-null>)`, comparisons and boolean operators whose inputs are all non-null, `EXISTS(…)`, casts of non-null values, and so on (the builder extends the list, each addition with tests).
   - Everything else is nullable. When the analyzer gives up, it records `analyzer-gave-up`, which `generate` reports as a note so the author can add an override.
   - Alias overrides (`"x!"`, `"x?"`) always win.
4. **Parameter required inference.** Strict. Default nullable. A parameter becomes required only when *every* use of it is one of:
   - a value inserted or updated into a `NOT NULL` column that has no default;
   - `<notnull column> = :p` in a WHERE or JOIN condition reached only through AND (not under OR, not under NOT).
   - Any use in `IS NULL`, `IS NOT NULL`, `COALESCE`, `NULLIF`, `IS DISTINCT FROM` and so on keeps it nullable. `:p!` and `:p?` always win.
5. Error positions from Postgres (character offsets) are mapped back to `line:col` in the source file.

### 8.2 SQL Server
- Parameters: `sp_describe_undeclared_parameters`. If it fails or is ambiguous, report the parameter by name. The fix is a one-line header override: `@param-type :p -> int` (exact syntax is the builder's call).
- Result columns, including nullability: `sp_describe_first_result_set` (its `is_nullable` comes from the server, outer joins included). No SQL parsing needed. ScriptDom is noted as a fallback if one is ever needed.
- Parameter syntax is `@param`, T-SQL native.
- The original Queryfirst used both procedures successfully. The new tool keeps the parameter handling but drops the design-time block.

## 9. `qf test`: compatibility rules

`qf test` reads the committed meta files, describes each embedded statement against the target DB, builds the meta it would generate there, and compares the two. Identical → pass. Every difference is reported; only breaking differences fail (the table below). `--strict` fails on any difference.

For "identical" to mean anything, **meta files hold nothing environment-specific**: type *names* (e.g. `public."accessLevel"`), never OIDs; no database name, host or server version. Two databases with the same schema produce byte-identical meta. Columns are matched **by name, never by position.**

| Change in target DB vs meta | Verdict |
|---|---|
| Extra result column | OK (note) |
| Expected column missing | **Error** |
| Column type changed | **Error** |
| Column non-null → nullable | **Error** |
| Column nullable → non-null | OK |
| Parameter added, removed, or changed type | **Error** |
| Query fails to prepare | **Error** |
| DB `timezone` ≠ `UTC` when config says timestamps are UTC (§13) | **Error** |

At generate time, two result columns with the same name are an error ("alias one of them"). Generated readers in every language map by name, so an app already deployed keeps working when columns are added.

Test output: a detailed per-query **semantic diff** (e.g. `getInvoice: column "total" numeric(12,2) → numeric(14,2)`, `parameter :id int4 → int8`, `column "note" became nullable`), each line marked error or warning, with `--json`. Prod databases can diverge from dev very subtly; the report must make every divergence visible. Exit code non-zero on any error (on any difference with `--strict`).

### 9a. `qf test` in place, in the deployed environment

The most reliable test is the real one: after deployment, against the datasource the live app is configured with. `qf test` must get that connection without knowing anything about a project's secrets setup.

- **Connection resolution** (*my proposal; the builder confirms the details*). The first match wins:
  1. `--db <connection string>`
  2. `test.connection.env` in config: the name of an environment variable holding a connection string, or the standard `PG*` variables if not set. This is enough when the deployed container already has the app's env.
  3. `test.connection.command` in config: a command that prints a connection string on stdout, like git's credential helpers. The project writes a small script (for simplyfirst, a Node module that reuses the app's own secrets loading) and Queryfirst simply runs it. This is the hook for "a module or a config file" without Queryfirst knowing anyone's secrets file.
  - The connection string is never logged and never written to disk.
- **Meta files must ship with the deployment.** A Next.js build doesn't naturally include `.qf.json` files from the source tree. `qf bundle` writes one `queryfirst.manifest.json` containing every meta file. The build copies it into the image, and `qf test --manifest <file>` reads it. `qf` itself must also be available in the deployed image: it's a dependency of the app, or the static binary is copied in.
- **Health check integration:** simplyfirst's blue/green health check runs `qf test` in green and treats a non-zero exit as unhealthy. It must be fast (describe only, in parallel) and read-only, and its output must be safe to log.

## 10. CLI

Binary name `qf` (*my call*; the npm package is `queryfirst`).

| Command | Purpose |
|---|---|
| `qf generate [paths…]` | **Dev DB.** Describe the queries in scope, write meta files and wrappers. Paths narrow the scope; no paths means the whole project. |
| `qf describe [file] [--sql <text>] [--db <conn>]` | **Any DB, writes nothing, no verdict.** Runs the generate pipeline for one query (a file, `--sql`, or stdin) and prints the result: params and columns with types, nullability and required, each **with its reason**. Default output is compact text; `--json` prints the same shape as a meta file. For drafting SQL before it exists as a file, for answering "why is this nullable?", and for inspecting one query against another DB (e.g. prod). |
| `qf check [paths…]` | **No DB, ever.** Hash-based consistency: `.sql` ↔ meta, meta ↔ wrapper, hand edits, recorded broken queries. Build step / CI. Never uses timestamps. |
| `qf test [--db <conn>] [--manifest <file>] [--strict] [paths…]` | **Target DB.** §9 and §9a. Uses only meta files (from the tree or a manifest). |
| `qf bundle [-o file]` | Write every meta file into one `queryfirst.manifest.json` for deployment (§9a). |
| `qf schema [tables…]` | No args: list tables and views. With args: columns (type, nullable, default), primary keys, foreign keys, and Postgres enums used. Compact text, with `--json`. Functions, triggers, indexes and permissions are out of scope. |
| `qf init [--from-pgtyped]` | Create config. With `--from-pgtyped`, converts `pgtyped-config.json` / `.pgtyped.ts`. |

Output rules, written for agents:
- Diagnostics look like `path:line:col: error[QFnnn]: message`, followed by an optional `hint:` line. Messages use schema terms, e.g. `column "emial" does not exist on "users"; did you mean "email"?` The tool computes the "did you mean" itself from the catalog.
- `--json` emits one machine-readable document (or JSON lines) with the same content.
- Exit codes: 0 = ok, 1 = query, check or test errors, 2 = usage, config or connection error.
- No interactive prompts, ever. No spinners when output isn't a TTY.
- If the dev DB can't be reached (for simplyfirst, usually because the SSH tunnel is closed), fail fast with exit 2 and a clear message (e.g. `cannot reach dev database at 127.0.0.1:25071; is the tunnel open?`). An agent can then ask a human, rather than retrying or guessing. simplyfirst-app's AGENTS.md rule ("never run pgtyped") gets replaced by "run `qf generate`; if the DB is unreachable, ask" (*my call*).
- Every run re-describes everything in scope. Describes run in parallel over a connection pool (size configurable).

The tool ships an `AGENTS.md` for its own repo and a snippet that consuming projects paste into theirs. In the spirit of Octopus's: one rule up front ("don't read generated code; read the `.qf.json`; run `qf generate` after editing a query"), then the commands.

### 10a. Broken queries: three channels

The tool owns all writes (names, determinism, hashes, orphan cleanup). When `generate` finds a broken query:
1. **Command output.** The diagnostic is printed with the exit code. This is the primary channel; it's what the agent who ran the command reads.
2. **Meta file.** The `.qf.json` records the query as broken, with its diagnostics. A later agent, or `qf check` in CI, sees the state without a DB.
3. **Wrapper.** The **last good types are kept**, so call sites don't explode with errors, and one deliberate compile error is added that carries the message, e.g. `export const queryfirstError: never = "getInvoice: column \"vat_rate\" does not exist on \"invoices\"";`. `tsc`, the IDE and CI fail exactly once, in the generated file, with the real message. If a query has no previous good wrapper, the wrapper contains only the error. Each generator defines its language's equivalent.

All three clear on the next good run. Wrappers are also deleted when their `.sql` disappears.

## 11. TypeScript generator (phase 1: pgtyped drop-in)

Output must be **equivalent to pgtyped's** for simplyfirst-app, so all ~800 call sites compile unchanged:
- File: `{{dir}}/{{name}}.qf.ts` (one query per file, so singular; template configurable, as in pgtyped's `emitTemplate`). This is the one deliberate break from pgtyped's file layout. Imports change from `…/x.queries` to `…/x.qf`, which the migration codemod rewrites mechanically (§12).
- Same export names: `I<Name>Params`, `I<Name>Result`, `I<Name>Query`, `export const <name> = new PreparedQuery<…>(<name>IR)`.
- Same helper types (`Json`, `DateOrString`, `stringArray`, `numberArray`, `NumberOrString`, `JsonArray`, Postgres enums as string unions such as `orderKind`), the same `void` result for statements that return nothing, and the same IR JSON shape (`usedParamSet`, `params[].locs`, `statement`) for `@pgtyped/runtime`.
- Same parameter typing: nullable params as `?T | null | void`, required as `T`.
- Honour `typesOverrides` (simplyfirst uses `numeric → number`) and `camelCaseColumnNames`.
- Line endings: LF, always.

**Intended differences from pgtyped** (expected to show up in the migration diff):
- More accurate result nullability. Narrowing (`T | null` → `T`) is compile-safe; widening (a LEFT JOIN column becomes nullable) causes compile errors that point to real bugs.
- Some parameters inferred as required (§8.1.4). Callers that then fail to compile were passing `undefined` into a NOT NULL comparison or insert, which is a bug.
- `@type` and `@returns` only apply where authors add them.

**Acceptance test for phase 1:** run on a copy of simplyfirst-app. Every query generates. A diff report against the current pgtyped `.queries.ts` files (compared content to content, ignoring the rename) lists every difference, grouped by kind. `tsc` passes after the migration. Each new compile error is resolved in one of three ways:
- **Real bug:** documented, and fixed or listed for the team.
- **Conservative analyzer:** the result is correct-but-cautious, for example nullable where the author knows better. The builder adds an override (`"x!"`, `:p?`) to the `.psql` file. This is expected migration work. *(My call.)*
- **Wrong analyzer:** a claim that is actually false (non-null when it can be null, or required when null is legitimate). The analyzer gets fixed, with a fixture.

## 12. Migration of simplyfirst-app

1. `qf init --from-pgtyped`.
2. Codemod: split the 26 multi-query `.psql` files into one file per query (file named after the kebab-cased `@name`). Rewrite every import of `…/x.queries` to `…/x.qf` across the app (about 840 generated files are imported), then delete the old `.queries.ts` files once generation succeeds. This can be a `qf` subcommand or a one-off script (*builder's call*).
3. `qf generate`, review the diff report, fix the bugs it reveals.
4. Remove pgtyped from the build.
5. Later, incrementally: add `@type` and `@returns`, then delete the hand-corrected `I*Corrected` interfaces (165 files) and the `as` casts (215) they make redundant. Agents can do this query by query.

## 13. Phase 2: wire types, rich types, and conversion

Context: simplyfirst's API routes are thin. Query results go over the wire unchanged, and all the work happens on the client. Dates and large numbers currently arrive as strings and get converted ad hoc at the point of use.

- Per query, generate a **wire type** (what JSON actually carries: dates and unsafe numbers as strings) and a **rich type** (dates as date types, safe numbers as `number`). `I<Name>Result` stays as it is for drop-in compatibility. The new types' names are the builder's call.
- Per query, generate a compact **conversion descriptor** (`{ createdAt: "timestamp", amount: "int8-string", … }`), plus a small **client-safe** runtime library `convert(descriptor, rows)` that touches only the fields listed. The descriptor and the library must be importable on the client without pulling in `pg` or `@pgtyped/runtime`, so the descriptor goes in a separate generated module.
- **Top-level columns are converted from schema information only. Never sniffed.**
- Mapping from DB type to rich type (configurable per project):
  - `date` → `DateWithoutTime` from epoq (github.com/bbsimonbb/epoq)
  - `timestamptz` → `Date`
  - `timestamp` (without time zone) → `Date`, read as **UTC** (project setting `timestamp: "utc"`; simplyfirst is UTC end to end). `qf test` checks the DB `timezone` is `UTC` (§9).
  - `int8` / `numeric` → `number` only when it's provably safe: a `CHECK` constraint bounds the column within ±(2^53−1), or `numeric(p,s)` with p ≤ 15. Otherwise the value stays a string. `qf test` reports an error if a constraint relied on here disappears.
- **Inside JSON values:**
  1. *Inferred shape (default).* Where the analyzer can follow `json_build_object`, `jsonb_build_object`, `json_agg`, `row_to_json(t)` and `to_jsonb(t)` back to typed columns, nested keys are converted according to their real types. Shapes it can't follow are recorded as unknown.
  2. *Declared shape.* A `@type` on a JSON column whose app type has its own converter.
  3. *Sniffing.* Opt-in per column, and only for the full `timestamptz` form (`YYYY-MM-DDTHH:MM:SS[.fff]±HH:MM`). Date-only and time-zone-less strings are never sniffed.
  - Large numbers inside JSON are silently rounded by `JSON.parse`, both in node-postgres on the server and on the client. Where the shape is known and a number is unsafe, `generate` recommends a `::text` cast in the SQL.
- **simplyfirst-app's current `pg` type parsers** (in `next.config.js`): `NUMERIC → Number(val)` (silent rounding beyond 15 digits), `DATE →` raw string, `TIMESTAMP → val.replace(" ", "T") + "Z"` (already UTC, as ISO strings), `INT8` left as a string (the BigInt parser is commented out). Milestone 1 leaves these untouched, so its types stay pgtyped-equivalent, `numeric → number` included. Whether phase 2 replaces these parsers and owns conversion end to end gets decided when phase 2 is designed. Either way, the wire types must describe whatever these parsers actually produce.

## 14. Configuration

- File: `queryfirst.json` at the project root (*my call*), with a JSON schema.
- Contents: provider; include/exclude globs (simplyfirst must exclude `sql-migration/`); generators and their options (emit template, `typesOverrides`, `camelCaseColumnNames`); connection pool size; phase 2 type-mapping settings.
- Connection: never stored in the file. Postgres reads the standard `PG*` environment variables (as `.pgtyped.ts` does today) or `DATABASE_URL`. The config can name a different environment variable. `--db` overrides it for `qf test`. SQL Server reads a connection-string environment variable. `qf test` resolves its connection as described in §9a (`test.connection.env` / `test.connection.command`).

## 15. Distribution

- npm package `queryfirst` (unclaimed as of 2026-10-30), with per-platform binary packages as optionalDependencies (Biome-style). It installs a `qf` bin. An unrelated npm package already uses the name `qf`, so docs say `npx queryfirst` for one-off use, and `qf` once the package is installed as a dev dependency.
- GitHub release binaries: win-x64, linux-x64, linux-arm64, darwin-x64, darwin-arm64.
- Rust's minimum supported version and the crate choices are the builder's call.

## 16. Testing

- A nullability and parameter-inference fixture suite: a table of query → expected meta, run against a real Postgres in a container. Each analyzer rule comes with fixtures, including negative ones.
- Golden tests for generator output, comparing pgtyped-compatible output byte for byte on a sample corpus.
- Determinism test: a second run writes nothing.
- The phase 1 acceptance run on simplyfirst-app (§11).
- SQL Server tests against a container. The existing `tests/TestTargets/SqlServerTestTarget/Queries` corpus in this repo is a starting point.

## 17. Bookmarked (not designed)

- **Index inspection.** While describing, run `EXPLAIN` (not `ANALYZE`) for queries, know the filter and join columns, and report likely missing indexes, possibly suggesting the `CREATE INDEX`.
- (Moved: the C# and TypeScript generators for SQL Server are now a planned milestone, §2a.)
- A long-running mode or language server, where a reactive runtime such as Octopus might fit.
- **MCP server (`qf mcp`).** A thin layer over the same engine, serving `describe`, `schema`, `generate` and `check` as typed tools over stdio, and possibly tables and meta files as resources. What it adds over the CLI: agents discover the tools without reading AGENTS.md, and a warm connection and catalog make repeated `describe`/`schema` calls near-instant (this is where the long-running mode above would live). The CLI stays the core: it works from any agent, in CI and in the deploy health check. Costs: a second surface to maintain, configuration in every client, and connection secrets in that client's config.

## 18. Decisions

**Made by Simon:**
- Lightweight cross-platform CLI; agentic development is the focus; Simplyfirst is the first user.
- Deep 100% re-description on every run; DB refactors must surface every break.
- Adding a column must not be a breaking change.
- One query per file.
- Parse with libpg_query; use `sp_describe_first_result_set` and `sp_describe_undeclared_parameters` on SQL Server.
- Nullable unless proven otherwise; overrides for the rest.
- Portability decides the language → Rust. No Octopus.
- Schema inspection included. Index inspection bookmarked.
- Drop-in replacement for pgtyped output, or machine-convertible. Generated files are named `.qf.ts` (one query per file); wrappers are `<name>.qf.ts`, meta `<name>.qf.json`: recognisable by extension, one family. Imports are rewritten by codemod.
- Parameters default to nullable; the author decides; required is inferred only where reliable.
- Adopt pgtyped's annotation syntax; add `@type` (including JSON app types) and `@returns`; `@name` optional.
- Conversion (wire/rich types) comes after the drop-in and is designed as described in §13; never sniff top-level columns; epoq for `date`; all timestamps UTC.
- Postgres + TypeScript first; SQL Server stays in scope and needs both C# and TypeScript generators.
- Name: Queryfirst, always with a small f. npm package `queryfirst`. New repo, folder `queryfirst-ai` where names clash.
- No database in CI. `qf test` runs in place after deployment, against the live app's datasource, as part of blue/green and the health check. It must work without knowing a project's secrets setup.

- `qf describe`: the generate pipeline for one query, printing the result without writing anything and without a verdict.
- Command names: `check` (no DB), `generate` (dev DB), `test` (target DB). `test` fails only on breaking differences, reports all, `--strict` for any difference.
- Broken queries: error printed, recorded in the meta file, and surfaced as a single compile error in the wrapper (last good types kept).
- This repo stays maintained as the VS extension.

**My calls (open to correction):**
- Meta file per query (`foo.qf.json`) next to the source, rather than one combined file.
- The SQL is embedded in the meta file so `qf test` needs only meta files.
- Built-in TypeScript generator in Rust; external generators over a stdin/stdout protocol.
- Binary `qf`, npm package `queryfirst`, config file `queryfirst.json`.
- `:p?` added as an explicit nullable override.
- `@returns` semantics (§6.4) and keeping `@pgtyped/runtime` in phase 1.
- Hand-edit detection uses hashes stored in the meta file, so the generated `.qf.ts` stays close to pgtyped's.
- `qf test` connection resolution: `--db`, then an env var, then a connection-helper command (§9a). Plus `qf bundle` / `--manifest` for deployment, and the offline `qf check` for CI.
- Agents run `qf` against the dev DB through the tunnel, and ask a human if it is unreachable.
- The TypeScript generator separates type generation from the runtime binding, ready for SQL Server.

## 19. Left to the builder
- The exact meta JSON schema, diagnostic codes, and the generator protocol envelope.
- Crate choices, concurrency model, and the internal module layout.
- The SQL Server parameter-type override syntax.
- Whether the multi-query split is a subcommand or a script.
- Naming of the phase 2 wire/rich types.
- Extending the nullability allowlist (with tests).
- Details of `qf test`'s connection helper and manifest format (§9a), and how `qf` gets into the deployed image.
- The Queryfirst runtime package name (scope availability).
