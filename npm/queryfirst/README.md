# queryfirst

Queryfirst (`qf`): write SQL in `.sql` files, get typed code generated for your language.

```sh
npm install --save-dev queryfirst
npx qf init
npx qf generate
```

This package is a thin launcher. The native `qf` binary ships in a per-platform package
(`queryfirst-win32-x64`, `queryfirst-linux-x64`, `queryfirst-linux-arm64`,
`queryfirst-darwin-x64`, `queryfirst-darwin-arm64`) that npm installs automatically as an
optional dependency. Linux binaries are static (musl) builds.

Also shipped:

- `queryfirst.schema.json`: JSON Schema for `queryfirst.json` (`qf init` references it).
- `qf-meta.v1.schema.json`: JSON Schema of the metadata that `qf` passes to external generators, for generator authors.

For the TypeScript runtime helpers used by generated code, see `queryfirst-runtime`.
