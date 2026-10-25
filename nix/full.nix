{
  lib,
  symlinkJoin,
  makeWrapper,
  openmig,
  postgresql,
  clickhouse,
}:
symlinkJoin {
  name = "openmig-full-${openmig.version}";
  paths = [openmig];
  nativeBuildInputs = [makeWrapper];
  postBuild = ''
    wrapProgram $out/bin/openmig \
      --suffix PATH : ${lib.makeBinPath [postgresql clickhouse]}
  '';
  meta =
    openmig.meta
    // {
      description = "OpenMig with PostgreSQL and ClickHouse binaries for scratch servers";
    };
}
