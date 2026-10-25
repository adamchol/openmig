{
  rustPlatform,
  openmig,
  postgresql,
  clickhouse,
  git,
}:
# Every test of the workspace, including the ones that start PostgreSQL and ClickHouse servers.
rustPlatform.buildRustPackage {
  pname = "openmig-integration-tests";
  inherit (openmig) version src cargoDeps;

  buildType = "debug";
  dontCargoBuild = true;
  nativeCheckInputs = [
    postgresql
    clickhouse
    git
  ];
  cargoTestFlags = [
    "--workspace"
    "--no-fail-fast"
  ];

  preCheck = ''
    export HOME=$TMPDIR
    export XDG_CACHE_HOME=$TMPDIR/cache
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p $out
    runHook postInstall
  '';
}
