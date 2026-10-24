# OpenMig

Declarative and versioned schema migrations for PostgreSQL and ClickHouse, written in Rust.

## Layout

* `crates/` — flat Cargo workspace. See `docs/spec/architecture.md` for what each crate owns.
* `docs/spec/` — the specification: `hcl.md` (schema language), `config.md` (`openmig.toml`),
  `cli.md`, `formats.md` (migration files, `openmig.sum`, history table), `architecture.md`
  (flows, normalization, planning rules). Keep it in sync with behaviour changes.
* `nix/` — package derivation. `flake.nix` exposes `packages`, `checks` and `devShells`.

## Working

* Enter the dev shell with `nix develop` (Rust 1.95, PostgreSQL 17, ClickHouse, cargo-insta).
* `cargo test --workspace` runs unit tests and the integration tests that start real servers.
  Integration tests find `initdb`/`postgres`/`clickhouse` on `PATH`.
* Before committing: `cargo fmt --all`, `cargo clippy --workspace --all-targets -- --deny warnings`,
  `cargo test --workspace`. `nix flake check` runs the same in the sandbox.
* Snapshot tests use insta: `cargo insta review` or `INSTA_UPDATE=always`. Golden plan cases are
  re-blessed with `OPENMIG_BLESS=1`.

## Conventions

* Edition 2024; errors with `thiserror` in libraries, `anyhow` only in the binary.
* No comments that narrate what code does; comment only non-obvious reasons and invariants.
* Generated SQL quotes and qualifies every identifier.
* Output must be deterministic: iterate `BTreeMap`/`IndexMap`, never `HashMap`, when producing plans,
  HCL or messages.
* Database servers only ever run as child processes started by tests or by `openmig-scratch`;
  never as system services.
* Commits follow conventional commits (`feat(postgres): ...`, `fix(hcl): ...`).
