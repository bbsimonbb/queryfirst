# AGENTS.md snippet for projects that use Queryfirst

Paste the section below into the consuming project's `AGENTS.md` (or `CLAUDE.md`). If the
project runs `qf` through wrapper scripts (an SSH tunnel, `npm run …`), name those scripts next
to each command, as simplyfirst-app does (`npm run qf -- <file>`, `npm run qf:db -- schema …`).

```markdown
## Database queries (Queryfirst)

SQL queries live in `.psql` / `.sql` files, one query per file; the file name gives the query its
name (`get-invoice.psql` exports `getInvoice`). Queryfirst (`qf`) describes each query against
the dev database, without ever running it, and writes two files next to it: `<name>.query.ts`
(the TypeScript wrapper) and `<name>.qf.json` (the meta file).

- Don't read generated `*.query.ts`; read the `.qf.json`. It lists the parameters and result
  columns with their types, nullability and required flags, and any diagnostics.
- After editing a `.psql`, run `qf generate <file>`. Never edit generated files by hand.
- If the database is unreachable (`QF090`), ask a human. Don't retry in a loop, and don't
  write the generated files yourself.
- Use `qf schema <tables>` to see real columns, keys and enums before writing SQL.
- Use `qf describe --sql "…"` to try a query before writing the file, and `qf describe <file>`
  to see why a column is nullable or a parameter required.
- Overrides, only when you know better than the analyzer, with a comment saying why: alias a
  column `AS "x!"` (never null) or `AS "x?"` (nullable); write a parameter `:p!` (required) or
  `:p?` (nullable).
- `qf check` needs no database: it verifies that the generated files match their queries.
```
