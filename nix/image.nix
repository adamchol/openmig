{
  lib,
  dockerTools,
  cacert,
  runCommand,
  openmig,
}:
let
  certs = "${cacert}/etc/ssl/certs/ca-bundle.crt";
  baseConfig = {
    Entrypoint = [ "${openmig}/bin/openmig" ];
    Env = [
      "SSL_CERT_FILE=${certs}"
      "OPENMIG_LOG_FORMAT=json"
    ];
    User = "65534:65534";
    WorkingDir = "/";
  };
in
{
  # The openmig binary alone; mount or copy migrations and pass flags yourself.
  base = dockerTools.buildLayeredImage {
    name = "openmig";
    tag = openmig.version;
    contents = [ cacert ];
    config = baseConfig;
  };

  # An image that applies `migrations` to the database in $OPENMIG_URL when run.
  mkMigrationsImage =
    {
      name,
      tag ? "latest",
      engine,
      migrations,
      waitForDb ? "2m",
      extraArgs ? [ ],
    }:
    let
      migrationsDir = runCommand "${name}-migrations" { } ''
        mkdir -p $out/migrations
        cp -r ${migrations}/. $out/migrations/
      '';
    in
    dockerTools.buildLayeredImage {
      inherit name tag;
      contents = [
        cacert
        migrationsDir
      ];
      config = baseConfig // {
        Cmd = [
          "deploy"
          "--engine"
          engine
          "--migrations"
          "/migrations"
          "--wait-for-db"
          waitForDb
        ]
        ++ extraArgs;
      };
    };
}
