# Queryfirst

Queryfirst validates `.sql` query files against Postgres and generates TypeScript wrappers.
The command-line tool is `qf`. Queries are only ever *described* (Parse/Describe over the
extended protocol), never executed.

Milestone 1 output is a drop-in replacement for [pgtyped](https://pgtyped.dev)'s
(`@pgtyped/runtime` stays the runtime), with more accurate nullability. It was accepted on a
real 953-query project; see [docs/acceptance/2026-milestone-1-simplyfirst.md](docs/acceptance/2026-milestone-1-simplyfirst.md).

## Install

The npm package is `queryfirst` (not published yet). Once it is: `npm i -D queryfirst`, then
`npx qf …` (an unrelated npm package is called `qf`, so use `npx queryfirst …` for one-off runs).
Until then, build it (below) and run `target/release/qf`.

## Quick start

```
qf init                 # writes queryfirst.json (qf init --from-pgtyped converts a pgtyped config)
qf generate             # describe every query against the dev database; write .qf.json + .query.ts
qf check                # offline: are the generated files up to date? (CI)
```

For each query file `get-invoice.psql`, `qf generate` writes, next to it:

- `get-invoice.qf.json`, the meta file: parameters and result columns with their Postgres types,
  nullability (with the reason), required flags, annotations and diagnostics. This is what
  other tools and agents read.
- `get-invoice.query.ts`, the TypeScript wrapper: `IGetInvoiceParams`, `IGetInvoiceResult`,
  `IGetInvoiceQuery` and `export const getInvoice = new PreparedQuery<…>(…)`, as pgtyped writes
  them.

Commit both. Files are written with LF line endings and only when their content changes, so a
second run writes nothing. On Windows with `core.autocrlf=true`, add a `.gitattributes`:

```
*.query.ts text eol=lf
*.qf.json text eol=lf
```

(`qf init` prints these lines too). Without them a fresh checkout turns the files CRLF; `qf
generate` and `qf check` compare text with line endings normalised, so that is not a change and
nothing is rewritten, but the files stay CRLF on disk.

Query files may use LF or CRLF line endings and may start with a UTF-8 byte order mark: both are
ignored, by the parser and by the hash recorded in the meta file. A file with no SQL statement
(empty, or only a header comment) is an error (`QF001`).

## Commands

| Command | Database | What it does |
|---|---|---|
| `qf generate [paths…]` | dev | Describe the queries in scope; write meta files and wrappers. No paths: the whole project, which also deletes the output of deleted queries. |
| `qf describe [file] [--sql <text>]` | any | Describe one query (a file, `--sql`, or stdin) and print params and columns with types, nullability and the reason for each. Writes nothing. Read-only sessions. |
| `qf check [paths…]` | none | Hash-based consistency of query ↔ meta ↔ wrapper, hand edits and recorded broken queries. |
| `qf test [--manifest <file>] [--strict] [paths…]` | target | Re-describe every committed query against another database (e.g. production) and report every difference; fails on breaking ones. Read-only sessions. |
| `qf bundle [-o <file>]` | none | Write every meta file into one `queryfirst.manifest.json`, so `qf test --manifest` can run where the sources are not (a deploy step). It is a build artifact: don't commit it. |
| `qf schema [tables…]` | any | No args: list tables and views. With tables: columns, defaults, keys, foreign keys and enums. Read-only sessions. |
| `qf init [--from-pgtyped]` | none | Write `queryfirst.json`; never overwrites one. |

Every command takes `--project <dir>` and `--json` (machine-readable output). `--project`
defaults to the nearest ancestor holding `queryfirst.json`, except for `qf init` and
`qf test --manifest`, which default to the current directory (a new project, a deployment
image). Commands that connect take `--db <conn>`, and their human-readable output (on stderr)
names the database they used as `database: host:port/db` (never the user or password), so a run
against the wrong database is noticed.

Diagnostics read `path:line:col: error[QFnnn]: message`, then an optional `hint:` line. Exit
codes: `0` ok, `1` query, check or test errors, `2` usage, configuration or connection error.
`QF090` means the database cannot be reached: an agent should ask a human (open the tunnel),
not retry. Nothing ever prompts.

When a query breaks, `generate` reports it, records it in the meta file, and keeps the last good
types in the wrapper plus one deliberate compile error carrying the message, so `tsc` fails once,
in the generated file, with the real reason.

## Connection

The connection is never stored in the config. `qf generate`, `describe` and `schema` use
`--db`, else the variable named by `connection.env`, else `DATABASE_URL`, else the standard
`PG*` variables (`PGHOST`, `PGPORT`, `PGUSER`, `PGPASSWORD`, `PGDATABASE`, `PGSSLMODE`).
`sslmode` follows libpq (`disable`, `prefer` (default), `require`, `verify-ca`, `verify-full`);
`no-verify` (node-postgres' spelling) is also accepted: TLS required, the certificate never
verified. `sslrootcert` (in the URL or `key=value` string, else `PGSSLROOTCERT`) names a PEM file
of CA certificates: `verify-ca` and `verify-full` verify against it instead of the Mozilla roots,
and, as in libpq, `require` with a root certificate verifies too (`sslrootcert=system` means the
Mozilla roots). `verify-ca` and `verify-full` both check the host name. Other libpq options that
Queryfirst does not support (e.g. `sslcert`, `sslkey`) are an error naming the option.
Passwords are never printed. `qf test` resolves `--db`, `test.connection.env`, `DATABASE_URL` /
`PG*`, then `test.connection.command` (a command whose standard output is the connection string).

Note: `DATABASE_URL` wins over the `PG*` variables. A project whose environment sets both (an
app pool URL and a direct session for describes) should name the variable it wants in
`connection.env`, or make sure `DATABASE_URL` is not set for `qf`.

## Configuration: `queryfirst.json`

```json
{
  "$schema": "./node_modules/queryfirst/queryfirst.schema.json",
  "include": ["app/**/*.*sql"],
  "exclude": ["**/node_modules/**", "sql-migration/**"],
  "pool": 8,
  "generators": [
    {
      "name": "typescript",
      "emitTemplate": "{{dir}}/{{name}}.query.ts",
      "typesOverrides": { "numeric": "number" },
      "camelCaseColumnNames": false
    }
  ]
}
```

| Key | Default | Meaning |
|---|---|---|
| `provider` | `"postgres"` | The only provider in milestone 1. |
| `include` | `["**/*.psql", "**/*.sql"]` | Globs of query files, project-relative, `/` separators; `*` stays within a directory, `**` spans them. |
| `exclude` | `["**/node_modules/**"]` | Globs to skip. Replaces the default when given. |
| `connection.env` | — | Environment variable holding the dev connection string (then `DATABASE_URL` and `PG*` are not used). |
| `pool` | `8` | Connections; describes run in parallel up to this many. |
| `generators` | one TypeScript generator | An empty list writes meta files only. |
| `generators[].emitTemplate` | `"{{dir}}/{{name}}.query.ts"` | Output path; `{{dir}}` is the query's directory, `{{name}}` its file name without extension. |
| `generators[].typesOverrides` | — | Postgres type → TypeScript type, as in pgtyped (one type, or `{ "parameter": …, "return": … }`). |
| `generators[].camelCaseColumnNames` | `false` | Camel-case result column names. |
| `test.connection.env` / `.command` | — | Where `qf test` finds the target database. |
| `timestamp` | — | `"utc"`: `qf test` checks that the database time zone is UTC. |

## Query files and annotations

One query per file (`QF002` otherwise). The name comes from the file name, camel-cased
(`get-invoice.psql` → `getInvoice`); pgtyped's `/* @name getInvoice */` is still accepted, and a
`@name` that disagrees with the file name is a warning (`QF003`). The leading comment holds the
annotations; its other lines (and plain comments before it) become the query's description.

```sql
/* Invoices of an account, newest first.
   @returns many
   @type theme -> Themes from "@/types/user/theme" */
SELECT
    I.id,
    I.reference,
    C."companyName" as "customerName!",   -- "!": never null (I know better than the analyzer)
    I.notes          as "notes?",          -- "?": nullable
    A.theme
FROM invoices I
    left join customers C on C.id = I."customerId"
    join accounts A on A.id = I."accountId"
WHERE I."accountId" = :accountId!          -- ":p!": required
    and I."customerId" = :customerId?      -- ":p?": nullable (NULL matches nothing), not inferred required
ORDER BY I."createdAt" desc
```

- **Parameters**: `:name`, as in pgtyped. The lexer ignores `::casts`, strings, quoted
  identifiers, dollar quotes and comments. A parameter is inferred **required** only when every
  use is a value for a `NOT NULL` column without a default, or `<not-null column> = :p` reached
  through `AND` only; anything else stays nullable. `:p!` and `:p?` always win.
- **Result nullability**: a column is non-null only when it is provably so: a `NOT NULL` column
  not on the nullable side of an outer join (at any depth), or an expression on an allowlist
  (`count`, literals, `coalesce(…, <non-null>)`, `exists`, comparisons of non-null values, casts…).
  Everything else is nullable; when the analyzer gives up it says so (`QF020` note, with the hint
  to alias the column `"x!"`). `qf describe <file>` prints the reason for every column.
- **`@param` expansions** (pgtyped syntax): `@param ids -> (...)`, `@param user -> (name, age!)`,
  `@param users -> ((name, age)...)`.
- **`@type <column> -> <Type> from "<module>"`** (or `@type :param -> …`, `Type[]` for arrays):
  use an application type; nullability still applies on top. The DB type stays in the meta
  file and `qf test` still checks it.
- **`@returns one | maybe | many | none`**: absent means pgtyped's array. `one` returns the row
  (throws unless exactly one), `maybe` the row or `null`, `none` nothing. These wrap the query with
  the small `queryfirst-runtime` package, which the project then depends on.

## Migrating from pgtyped

1. `qf init --from-pgtyped` converts `pgtyped-config.json` or `.pgtyped.ts` (the `db` section is
   dropped: the connection comes from the environment, as before).
2. `node tools/migrate-pgtyped/migrate.mjs <project> --dry-run`, then without `--dry-run`: splits
   multi-query files into one file per query and rewrites imports of `x.queries` to `x.query`
   (see [tools/migrate-pgtyped/README.md](tools/migrate-pgtyped/README.md)).
3. `qf generate`, compare with the old `.queries.ts`, delete them, run `tsc`. New compile errors
   are expected: where a column became nullable or a parameter required, the caller was either
   wrong (fix it) or relies on something the analyzer cannot see (add `"x!"` / `:p?` with a
   comment).
4. Replace pgtyped in the build; keep `@pgtyped/runtime`. Paste [docs/agents-snippet.md](docs/agents-snippet.md)
   into the project's `AGENTS.md`.

## Building

Requires the Rust toolchain pinned in `rust-toolchain.toml` (installed automatically by rustup).

```
cargo build --workspace
cargo test --workspace
```

The `pg_query` crate compiles the Postgres parser and generates bindings with bindgen, so it needs
LLVM/libclang.

### Windows

1. Install LLVM (for example `winget install LLVM.LLVM`).
2. Point bindgen at it: `LIBCLANG_PATH="C:\Program Files\LLVM\bin"`.
3. Build from a Visual Studio developer prompt (or otherwise make the MSVC and Windows SDK headers
   visible via `INCLUDE`). From a plain shell bindgen fails with
   `'stdbool.h' file not found` and then `'sys/types.h' file not found`. In that case pass the
   include directories explicitly, for example:

```
export LIBCLANG_PATH="C:\Program Files\LLVM\bin"
export BINDGEN_EXTRA_CLANG_ARGS='-I"C:/Program Files/LLVM/lib/clang/<ver>/include" -I"<MSVC>/include" -I"<WindowsKits>/Include/<sdk>/ucrt" -I"<WindowsKits>/Include/<sdk>/shared" -I"<WindowsKits>/Include/<sdk>/um"'
```

### Linux / macOS

Install libclang (`sudo apt-get install libclang-dev`; on macOS the Xcode command line tools suffice).

## Database tests

```
export QF_TEST_DATABASE_URL=postgres://postgres:postgres@localhost:5432/postgres
cargo test --workspace --features db-tests
```

## License

MIT
