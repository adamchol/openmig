# The default dev shell. `dnvr up` starts PostgreSQL and ClickHouse plus OpenMig keeping the example
# project in examples/dev in sync with its schema files; edit them and watch the panes.
{ pkgs, presets }:
{ config, ... }:
let
  example = "$DNVR_ROOT/examples/dev";
  openmig = ''cargo run --quiet --release --manifest-path "$DNVR_ROOT/Cargo.toml" -p openmig --'';
in
{
  description = "OpenMig development: `dnvr up` runs PostgreSQL, ClickHouse and `sync --watch` on examples/dev";

  packages = with pkgs; [
    cargo
    rustc
    rustfmt
    clippy
    rust-analyzer
    cargo-insta
    postgresql_17
    clickhouse
    nixfmt
  ];

  env = {
    RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
    OPENMIG_DEV_PG_URL = config.processes.pg.socketUrl;
    OPENMIG_DEV_PG_PROD_URL = "postgresql://${config.processes.pg.superuser}@/shop_prod?host=${config.processes.pg.socketPath}&port=${toString config.processes.pg.port}";
    OPENMIG_DEV_CH_URL = config.processes.ch.httpUrl;
  };

  processes = {
    pg = {
      imports = [ presets.postgres ];
      package = pkgs.postgresql_17;
      database = "shop";
      extraDatabases = [ "shop_prod" ];
      listenAddresses = "";
      runAsUser = "nobody";
    };

    ch = {
      imports = [ presets.clickhouse ];
      database = "analytics";
      httpPort = 18123;
      tcpPort = 19000;
      logLevel = "warning";
    };

    sync-pg = {
      env.READY = "dnvr://pg/database";
      command = ''${openmig} --config "${example}/openmig.toml" --db shop sync --watch --yes --allow-destructive'';
    };

    sync-ch = {
      env.READY = "dnvr://ch/database";
      command = ''${openmig} --config "${example}/openmig.toml" --db analytics sync --watch --yes --allow-destructive'';
    };
  };

  scripts = {
    om = {
      description = "Run openmig (built from this checkout) on the examples/dev project";
      text = ''exec ${openmig} --config "${example}/openmig.toml" "$@"'';
    };
    om-deploy-prod = {
      description = "Deploy the example's PostgreSQL migrations into the shop_prod database";
      text = ''exec ${openmig} --config "${example}/openmig.toml" --db shop --url "$OPENMIG_DEV_PG_PROD_URL" deploy "$@"'';
    };
    om-status-prod = {
      description = "Show migration status and drift of the shop_prod database";
      text = ''exec ${openmig} --config "${example}/openmig.toml" --db shop --url "$OPENMIG_DEV_PG_PROD_URL" status --drift "$@"'';
    };
  };
}
