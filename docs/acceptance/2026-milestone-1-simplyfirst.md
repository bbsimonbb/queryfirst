# Milestone 1 acceptance: simplyfirst-app migrated from pgtyped

Date: 2026-10-31. Spec: §11 (phase 1 acceptance) and §12 (migration).

> Note: after this acceptance the generated wrappers were renamed from `<name>.query.ts` to `<name>.qf.ts` (imports `…/x.qf`), so they share the `.qf` family with the meta file. Names below are as they were at acceptance.

The migration was done in a worktree of simplyfirst-app, branch `queryfirst-migration`
(from `main` at `bf1df9cdb`), and left there for review: nothing merged or pushed.

| Commit (simplyfirst-app, `queryfirst-migration`) | |
|---|---|
| `de1fe7ebc` | `qf init --from-pgtyped` + codemod (split files, rewrite imports) |
| `9e569f0e3` | generated `.query.ts` / `.qf.json`, old `.queries.ts` deleted, tsc errors resolved |
| `fea84eedc` | pgtyped out of the build; AGENTS.md / docs/conventions.md; `.gitattributes`, `.gitignore` |

| Commit (queryfirst-ai, `milestone-1`) | |
|---|---|
| `1d28c74` | fix: accept plain comments before the `@name` header |
| `5ceda73` | fix (codemod): send shared helper types to a split query that uses them |
| (this commit) | docs: this report, `docs/agents-snippet.md`, README user docs |

## Result

- **Every query generates**: 953 queries, 0 errors (16 `QF003` warnings, 29 `QF020` notes, below).
- **Diff report**: "other" is empty (after one Queryfirst fix).
- **`npx tsc --noEmit` passes** after resolving 38 errors: 0 wrong-analyzer, 3 conservative
  (overrides), 24 real bugs fixed at the call site, 5 real bugs listed for the team,
  5 invariants TypeScript cannot see, and 1 codemod bug.
- **`qf check`**: exit 0. **Second `qf generate`**: 0 files written, `git status` clean.
- **`qf bundle`** + **`qf test --manifest queryfirst.manifest.json`** against the dev database:
  exit 0, "953 queries, 0 with differences".
- Unit tests (vitest): 633 passed, 7 skipped, 2 failed. Both failures are unrelated to the migration
  (no file they cover changed): `modals/registry.test.ts` expects 80 modals and finds 81, and
  `lib/images/compress.test.ts` times out under parallel load (passes alone). 18 test files mock
  `.query` modules with `vi.mock`; all pass.

## Timings

All database figures go **through the SSH tunnel** (Windows 10 laptop → droplet → DigitalOcean
managed Postgres, fra1), pool 8. They are not local-database figures; the < 10 s local target
could not be measured here.

| Run | Time |
|---|---|
| `qf generate`, fresh project (953 queries, 1906 files written) | 15.2 s |
| `qf generate`, nothing to write (953 queries re-described) | 8.8 – 10.1 s |
| full `npm run pgt`-style run: tunnel up + env-cmd + generate + tunnel down | 11.4 s (no-op generate) |
| `qf test --manifest` (953 queries) | 8.3 – 9.5 s |
| `qf bundle` | 0.3 s |
| `qf check` (no database) | 0.76 s |
| `npx tsc --noEmit` (for scale) | 35 s cold, 15 s warm |

The very first full run was not timed precisely (the shell lacked `bc`); the "fresh project"
figure is the same run reproduced on a copy holding only the query files and `queryfirst.json`.
Its output was byte-identical to the worktree's.

## Codemod and init

- `qf init --from-pgtyped`: `include: ["app/**/*.*sql"]`, `typesOverrides: { numeric: number }`;
  three `QF092` notes (`db`, `failOnError`, `maxWorkerThreads` dropped).
- Codemod: 25 multi-query files split into 136 query files; 1224 imports rewritten in 527 files;
  0 for manual review. (`.pgsql` files, 13 of them, are in scope through `*.*sql` and work.)

## Diff report

Each new `.query.ts` compared with its pgtyped block (from the old `.queries.ts`, the split
file's original for split queries), ignoring the rename and CRLF; one old file had been
prettier-formatted and was compared after formatting both. Interfaces are compared field by
field; the IR (`usedParamSet`, `params[].locs`, `statement`) must be identical except for
`required` flags that follow a parameter change.

| Group | Items | Queries |
|---|---|---|
| identical | | 273 |
| nullability narrowed (`T \| null` → `T`) | 130 | 106 |
| nullability widened (`T` → `T \| null`) | 199 | 98 |
| param became required (`?: T \| null \| void` → `: T`) | 901 | 579 |
| param became optional | 0 | 0 |
| doc comment only | 1 | 1 |
| **other** | **0** | **0** |
| old queries with no new counterpart | 0 | |

- Narrowed: mostly `count(*)`, `exists(…)`, comparisons (`belongs`, `has…`, `is…`) and
  `coalesce(x, <not null>)`. Spot-checked with `qf schema` / `qf describe` (UNION branches,
  coalesce over accounts defaults): all correct.
- Widened: 108 `string`, 64 `number`, 11 `boolean`, 10 `Json`, 6 `Date`; columns from the
  nullable side of a LEFT JOIN that pgtyped reported as non-null.
- Required: 682 `number`, 164 `string`, 32 `DateOrString`, 20 `Json`, 2 `boolean`,
  1 `NumberOrString`: `<not null column> = :p` under AND, and values for NOT NULL columns
  without a default.
- Doc comment only: `get-quotes` gains the description from a plain comment that preceded its
  `@name` (spec §5), which pgtyped dropped.
- Before the fix, one query was missing (counted in "other"): see Queryfirst fix 1.

After the overrides below, 4 queries differ in their IR statement too, because their SQL text was
edited; that is the edit, not a generator difference.

## tsc errors and their resolution

38 errors in 27 files after deleting the `.queries.ts` files.

### Wrong analyzer: none

### Conservative analyzer: overrides added (3)

| File | Override | Why it is safe |
|---|---|---|
| `cron-jobs/invoice-reminders/(queries)/get-reminders.psql` | `"customerEmail!"` | the WHERE clause keeps only rows where `coalesce(LS.recipient, Co.email)` is not null |
| `cron-jobs/quote-reminders/(queries)/get-reminders.psql` | `"customerEmail!"` | same |
| `shared/quote/sign/otp/(queries)/get-sign-context.psql` | `"recipientEmail!"` | `sendQuoteTokens.emailId` is NOT NULL with an FK, so the LEFT JOIN to `emails` always matches |

The last one is a possible analyzer improvement (a LEFT JOIN on a NOT NULL foreign key behaves
as an inner join), not a wrong claim.

### Real bugs fixed at the call site (24 errors)

| Where | Bug | Fix |
|---|---|---|
| `cron-jobs/invoice-reminders/route.tsx` | `createToken` omits `isPaid`, a NOT NULL column without default: **every automatic invoice reminder fails its insert and rolls back** | `isPaid: false` (a reminded invoice is unpaid) |
| `cron-jobs/{invoice,quote}-reminders/route.tsx` (2) | the sender comes from a LEFT JOIN LATERAL; `createNotification` declares `:userId!`, so a reminder with no sender would fail | notify only when there is a sender |
| `cron-jobs/send-emails/route.tsx` | `cc` gets `null` when the email has no user (LEFT JOIN) | `userEmail ?? undefined` |
| 9 `settings/**` GET routes (10 errors) | no session guard: an anonymous request reaches the query with `accountId: undefined` (404 instead of 401) | `if (!session?.user) return 401`, as their PUT handlers do |
| 4 `editors/*/[id]/pricing` routes (8 errors) | `body.chiffrageId` (optional in `IRow`) reaches `id = :pricingId` unvalidated | 400 when it is missing |
| `auth/sign-up/route.ts` | ambassador looked up with a `null` code (never matches) | skip the lookup without `ref` |
| `customers/(index)/route.ts` | account manager looked up with a `null` id (never matches) | skip the lookup without an id |

### Real bugs listed for the team (5 errors), marked `TODO(queryfirst-migration)`

- `orders/[id]/create-quote`: free order lines have `chiffrageId` NULL (the query says so in a
  comment), so `getNewVariant` finds nothing and the route throws "No variant found": **creating a
  quote from an order that contains a free line fails**. Kept mechanical with `:chiffrageId?` in
  `get-new-variant.psql` (true: NULL simply matches nothing). Needs a product decision (copy free
  lines as free rows?).
- `editors/{deposit,final-invoice,manual-invoice,situation}-editor/[id]/finalize` (4):
  `releaseDate` (NOT NULL) comes from `editable.holdbackDate`, which the hand-written
  `IGetInvoiceCorrected` allows to be null. A holdback without a date would fail the insert. Kept
  with `holdbackDate!`; the team should confirm the invariant or validate it.

### Invariants TypeScript cannot see (5 errors), asserted with `!` and a comment

- `modals/(customer)/review-access-request/route.ts` (2): `customerId` is optional in the zod
  schema, but a `refine` requires it when the request is accepted (the file already asserts
  `refusalReason!` the same way).
- `pricings/internal/create-quote/route.ts`: `status` comes from a LEFT JOIN to `leads`, but the
  branch runs only when `pricing.email` is set, and `leads.email` / `leads.status` are NOT NULL.
- `shared/quote/sign/route.tsx` (2): sender e-mail and first name come from a LEFT JOIN to
  `User`, inside `if (ctx.senderId)`; `User.email` / `firstName` are NOT NULL.

### Codemod bug (1 error)

- `lib/events/publish.ts` imported `Json` from `get-subscribed-endpoints.query`, which does not
  export it. Fixed in the codemod (fix 2 below) and in the file.

## Queryfirst fixes made

1. **`qf-sqlfile`: plain comments before the `@name` header** (`1d28c74`). pgtyped accepts a
   comment before `/* @name … */`; `get-related-documents.psql` had one, which the split put on top
   of `get-quotes.psql`. Queryfirst took the first comment as the only header candidate, so the
   `@name` comment fell into the body and was reported as a second query (`QF002`). Leading plain
   comments now join the description when a tagged header follows. Test:
   `crates/qf-sqlfile/tests/header.rs::plain_comments_before_header`.
2. **Codemod: shared helper types** (`5ceda73`). A split `.query.ts` exports only the helpers its
   own query uses; the codemod pinned helpers to the import's first query group. It now reads which
   pgtyped blocks reference each helper. Test: `tools/migrate-pgtyped/migrate.test.mjs`
   ("shared helper types go to a query whose generated block uses them"). The fixed codemod, run
   dry on simplyfirst's `main`, produces exactly the hand-corrected `publish.ts`.

## Removing pgtyped from the build

- `package.json`: `pgt` / `pgt:file` → `qf` (`generate` through the tunnel; `npm run qf -- <file>`
  narrows it), `qf:db` (any `qf` command through the tunnel: `schema`, `describe`, `test`) and
  `qf:check` (offline). pgtyped's watch mode has no Queryfirst equivalent.
- The `queryfirst` npm package is not published (npm 404), so the scripts call
  `scripts/qf.mjs`, which runs `QF_BIN` or the local milestone-1 build. Marked
  `TODO(queryfirst-migration)`: add `queryfirst` to devDependencies and delete the shim once
  published.
- `@pgtyped/cli`, `.pgtyped.ts` and `pgtyped-config.json` removed. `@pgtyped/runtime` stays (the
  wrappers use it).
- `AGENTS.md`: the pgtyped section ("an AI must never run `npm run pgt`") is replaced by the agent
  snippet, adapted to the npm scripts: agents now run `npm run qf -- <file>` and ask a human on
  `QF090`. **This reverses the old rule; Simon should confirm it.** `docs/conventions.md` updated
  the same way; its GET route example now has the session guard the generated types require.
- `.gitattributes` keeps `*.query.ts` / `*.qf.json` LF (see below). `.gitignore` gets
  `queryfirst.manifest.json`: `qf bundle` rebuilds it from the committed meta files (5.6 MB), so it
  is a deployment artifact, not source.

## Left over / for follow-up

- **CRLF churn without `.gitattributes`**: with `core.autocrlf=true`, a fresh checkout makes the
  generated files CRLF; `qf check` accepts them but `qf generate` rewrites them (observed: 2 files
  written after re-checking out one query's outputs). Fixed in simplyfirst by `.gitattributes`.
  Queryfirst could compare modulo CRLF in `generate`, or `qf init` could write the
  `.gitattributes` lines; documented in the README for now.
- **`DATABASE_URL` wins over `PG*`** (connection resolution). pgtyped used only the `PG*`
  values, and simplyfirst's `pgt` script relies on `PG*` pointing at the direct (non-pooler)
  forward. Verified without reading any env file: with `PGPORT` pointed at a closed port, `qf`
  failed with `QF090` on that port, so no `DATABASE_URL` is set there. A project that sets both
  should use `connection.env`. Documented.
- pgtyped's `db.ssl: true` is dropped by `init`; `qf` defaults to `sslmode=prefer` (TLS when
  offered, no certificate check). The dev database negotiated TLS. Add `PGSSLMODE=require` to the
  scripts if TLS must be enforced.
- 16 `QF003` warnings: single-query files whose `@name` differs from the file name (e.g.
  `address.psql` → `addressBelongsToAccount`). Harmless; rename the files or drop `@name` later.
- 29 `QF020` notes: columns the analyzer could not prove non-null (computed dates, `json ->>`
  extracts, lateral subqueries). None caused a compile error; add `"x!"` where the author knows.
- Codemod over-match: it also rewrote a `"{{dir}}/{{name}}.queries.ts"` string literal (the
  `emitTemplate` in `.pgtyped.ts`), not just module specifiers. Harmless here (the file is
  deleted); worth anchoring the rewrite to import/export/mock positions.
- Untouched in simplyfirst: `.notes/` and `docs/superpowers/plans/` (historical), `.env.example`
  and `.env.development` comments mentioning `npm run pgt` (env files left alone), and
  `.claude/settings.local.json`.
- `qf test` was run against the dev database only, not production.
