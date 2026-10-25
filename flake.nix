{
  description = "OpenMig: declarative and versioned schema migrations for PostgreSQL and ClickHouse";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
  };

  outputs = {
    self,
    nixpkgs,
  }: let
    linux = ["x86_64-linux" "aarch64-linux"];
    darwin = ["aarch64-darwin" "x86_64-darwin"];
    forSystems = systems: f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
    forAllSystems = forSystems (linux ++ darwin);
    forLinux = forSystems linux;
  in {
    packages = forAllSystems (pkgs: let
      openmig = pkgs.callPackage ./nix/package.nix {};
    in
      {
        inherit openmig;
        # The binary plus PostgreSQL 17 and ClickHouse on PATH, so scratch servers work anywhere.
        openmig-full = pkgs.callPackage ./nix/full.nix {
          inherit openmig;
          postgresql = pkgs.postgresql_17;
        };
        default = openmig;
      }
      // nixpkgs.lib.optionalAttrs pkgs.stdenv.hostPlatform.isLinux {
        image = (pkgs.callPackage ./nix/image.nix {inherit openmig;}).base;
      });

    lib = {
      # Build an OCI image that deploys a migration directory:
      #   openmig.lib.mkMigrationsImage {
      #     inherit pkgs;
      #     name = "ghcr.io/acme/app-migrations";
      #     engine = "postgres";
      #     migrations = ./db/migrations;
      #   }
      mkMigrationsImage = {pkgs, ...} @ args:
        (pkgs.callPackage ./nix/image.nix {
          openmig = self.packages.${pkgs.stdenv.hostPlatform.system}.openmig;
        }).mkMigrationsImage (builtins.removeAttrs args ["pkgs"]);
    };

    checks = forLinux (pkgs: {
      inherit (self.packages.${pkgs.stdenv.hostPlatform.system}) openmig;
      integration = pkgs.callPackage ./nix/integration.nix {
        openmig = self.packages.${pkgs.stdenv.hostPlatform.system}.openmig;
        postgresql = pkgs.postgresql_17;
      };
      migrations-image = self.lib.mkMigrationsImage {
        inherit pkgs;
        name = "openmig-example-migrations";
        engine = "postgres";
        migrations = ./crates/openmig/tests/cases/postgres/tables/create;
      };
      formatting = pkgs.runCommand "check-nix-formatting" {nativeBuildInputs = [pkgs.alejandra];} ''
        alejandra --check ${./flake.nix} ${./nix}
        touch $out
      '';
    });

    formatter = forAllSystems (pkgs: pkgs.alejandra);

    devShells = forAllSystems (pkgs: {
      default = pkgs.mkShell {
        packages = [
          pkgs.cargo
          pkgs.rustc
          pkgs.rustfmt
          pkgs.clippy
          pkgs.rust-analyzer
          pkgs.cargo-insta
          pkgs.postgresql_17
          pkgs.clickhouse
          pkgs.alejandra
        ];
        RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
      };
    });
  };
}
