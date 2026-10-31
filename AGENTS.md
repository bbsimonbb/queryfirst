# Queryfirst — agent orientation

Queryfirst is a Rust CLI (`qf`) that validates `.sql` query files against a live Postgres and
generates TypeScript wrappers. It is a replacement for pgtyped.

**The one rule that saves you the most time:** **pgtyped at `c:\dev\pgtyped` is the authority on
output bytes.** When the question is "what exactly should the generated TypeScript (or the param
parsing behind it) look like?", read pgtyped's source and its fixtures there. Do not guess, and do
not "improve" the output.

## Crate map

| Crate | Role |
|---|---|
| `qf-sqlfile` | Query files, annotations, pgtyped-compatible param lexer / IR. Depends on nothing internal. |
| `qf-meta` | Meta model, hashing, diff. |
| `qf-pg` | Describe, catalog, analyzers. **The only crate that touches a database.** Depends on `qf-meta`, `qf-sqlfile`. |
| `qf-gen-ts` | TypeScript generator. Depends **only** on `qf-meta`. |
| `queryfirst` | The CLI (binary `qf`). Depends on all of the above and **owns all file writes**. |

The dependency rule is enforced by each crate's `Cargo.toml`; do not add edges that break it.

## Build and test

```
cargo build --workspace
cargo test --workspace                          # no database needed
cargo test --workspace --features db-tests      # needs QF_TEST_DATABASE_URL
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
```

`db-tests` tests require `QF_TEST_DATABASE_URL` (e.g. `postgres://postgres:postgres@localhost:5432/postgres`)
and **fail, not skip,** when it is missing.

`pg_query` needs libclang to build; on Windows see `README.md#building`.

Always write the name as "Queryfirst" (small f) in docs and help text.
