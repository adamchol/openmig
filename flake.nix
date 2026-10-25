{
  description = "OpenMig: declarative and versioned schema migrations for PostgreSQL and ClickHouse";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
    flake-parts.url = "github:hercules-ci/flake-parts";
    flake-parts.inputs.nixpkgs-lib.follows = "nixpkgs";
    dnvr.url = "github:dialohq/dnvr";
    dnvr.inputs.nixpkgs.follows = "nixpkgs";
  };

  outputs =
    inputs@{ self, flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];

      imports = [ inputs.dnvr.flakeModule ];

      perSystem =
        { config, pkgs, ... }:
        let
          inherit (pkgs) lib;
          openmig = pkgs.callPackage ./nix/package.nix { };
        in
        {
          formatter = pkgs.nixfmt;

          packages = {
            inherit openmig;
            default = openmig;
            # The binary plus PostgreSQL 17 and ClickHouse on PATH, so scratch servers work anywhere.
            openmig-full = pkgs.callPackage ./nix/full.nix {
              inherit openmig;
              postgresql = pkgs.postgresql_17;
            };
          }
          // lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            image = (pkgs.callPackage ./nix/image.nix { inherit openmig; }).base;
          };

          checks = lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
            inherit openmig;
            integration = pkgs.callPackage ./nix/integration.nix {
              inherit openmig;
              postgresql = pkgs.postgresql_17;
            };
            migrations-image = self.lib.mkMigrationsImage {
              inherit pkgs;
              name = "openmig-example-migrations";
              engine = "postgres";
              migrations = ./crates/openmig/tests/cases/postgres/tables/create;
            };
            formatting = pkgs.runCommand "check-nix-formatting" { nativeBuildInputs = [ pkgs.nixfmt ]; } ''
              nixfmt --check ${./flake.nix} ${./nix}/*.nix
              touch $out
            '';
          };

          dnvr.shells.default = {
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
            env.RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
          };
        };

      flake.lib = {
        # Build an OCI image that deploys a migration directory:
        #   openmig.lib.mkMigrationsImage {
        #     inherit pkgs;
        #     name = "ghcr.io/acme/app-migrations";
        #     engine = "postgres";
        #     migrations = ./db/migrations;
        #   }
        mkMigrationsImage =
          { pkgs, ... }@args:
          (pkgs.callPackage ./nix/image.nix {
            openmig = self.packages.${pkgs.stdenv.hostPlatform.system}.openmig;
          }).mkMigrationsImage
            (builtins.removeAttrs args [ "pkgs" ]);
      };
    };
}
