{
  description = "OpenMig: declarative and versioned schema migrations for PostgreSQL and ClickHouse";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-26.05";
  };

  outputs = {
    self,
    nixpkgs,
  }: let
    systems = ["x86_64-linux" "aarch64-linux"];
    forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});
  in {
    packages = forAllSystems (pkgs: {
      openmig = pkgs.callPackage ./nix/package.nix {};
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.openmig;
    });

    checks = forAllSystems (pkgs: {
      inherit (self.packages.${pkgs.stdenv.hostPlatform.system}) openmig;
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
