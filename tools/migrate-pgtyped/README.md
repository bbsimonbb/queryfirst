# pgtyped -> Queryfirst migration codemod

    node tools/migrate-pgtyped/migrate.mjs <project> [--dry-run]

Prepares a pgtyped project for Queryfirst (no dependencies, Node 20+).

1. Every `.psql`/`.sql` file holding more than one `/* @name ... */` query is split into one file per query, named
   `kebab(@name).psql` next to the original (the `@name` header is kept, so exported names do not change). The original
   is deleted. If a target file already exists the run aborts and writes nothing (exit 1).
2. In `**/*.{ts,tsx}` (excluding `node_modules`), imports/re-exports of `x.queries` become `x.query`. For a split file
   each imported symbol (`name`, `I<Pascal>Params|Result|Query`) is mapped to its query's module; an import pulling
   from several queries becomes several import lines (`import type` and `a as b` are preserved). `@/` aliases are
   resolved from `tsconfig.json` `paths` (fallback `@/*` -> `./*`). Line endings of each file are preserved.
3. Prints a summary. `*.queries.ts` files are not deleted; run `qf generate` and review first.

Not rewritten automatically (listed as MANUAL REVIEW): references to a split module that are not an import/export list
(`vi.mock(...)`, `require`, `import()`, namespace/default imports) and non-query symbols such as pgtyped's shared `Json` type.

`--dry-run` writes nothing. Exit codes: 0 success, 1 conflict, 2 usage error.

Tests: `node --test tools/migrate-pgtyped`. Set `QF_PGTYPED_CORPUS=<pgtyped project>` to add a read-only dry-run smoke test.
