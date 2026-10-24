{
  lib,
  rustPlatform,
  rustfmt,
  clippy,
}: let
  manifest = lib.importTOML ../crates/openmig/Cargo.toml;
in
  rustPlatform.buildRustPackage {
    pname = "openmig";
    version = manifest.package.version;

    src = lib.fileset.toSource {
      root = ../.;
      fileset = lib.fileset.unions [
        ../Cargo.toml
        ../Cargo.lock
        ../crates
      ];
    };
    cargoLock.lockFile = ../Cargo.lock;

    cargoBuildFlags = ["--package" "openmig"];
    nativeBuildInputs = [rustfmt clippy];

    # Integration tests spawn real PostgreSQL and ClickHouse servers and run as separate checks.
    cargoTestFlags = ["--workspace" "--lib" "--bins"];

    postCheck = ''
      cargo fmt --all --check
      cargo clippy --workspace --all-targets -- --deny warnings
    '';

    meta = {
      description = "Declarative and versioned schema migrations for PostgreSQL and ClickHouse";
      license = lib.licenses.asl20;
      mainProgram = "openmig";
    };
  }
