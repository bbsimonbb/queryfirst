# Queryfirst

Queryfirst validates `.sql` query files against Postgres and generates TypeScript wrappers.
The command-line tool is `qf`.

> Status: early scaffold. Subcommands (`generate`, `check`, `test`, `bundle`, `schema`, `init`)
> exist but print "not implemented" and exit with code 2.

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
