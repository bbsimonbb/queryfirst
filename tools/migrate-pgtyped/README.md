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

Not rewritten automatically (listed as MANUAL REVIEW): `require`, `import()`, namespace imports of a split module, unknown symbols,
and mocks whose factory names no known query.

`--dry-run` writes nothing. Exit codes: 0 success, 1 conflict, 2 usage error.

Tests: `node --test tools/migrate-pgtyped`. Set `QF_PGTYPED_CORPUS=<pgtyped project>` to add a read-only dry-run smoke test.

## Scope and extra rules

- Only files in scope are split: `--include/--exclude <glob>` (repeatable) override `queryfirst.json` (`include`/`exclude`),
  which wins over `pgtyped-config.json` (`srcDir` + sql transform `include`). The scope source is printed. With none of
  these the run refuses (exit 2). A multi-query file with no sibling `<name>.queries.ts` is always skipped and listed.
- pgtyped's shared helper types (`Json`, `DateOrString`, enum aliases, ...) in an import go with the first query group of
  that import (or the split's first query when only helpers are imported). A default import stays with the first group.
- `vi.mock`/`jest.mock` of a split module becomes one mock per target module whose query name is a key in the factory
  (an automock becomes one per target). Anything else is MANUAL REVIEW.
- `x.psql` and `x.sql` side by side with multiple queries conflict (exit 1).
