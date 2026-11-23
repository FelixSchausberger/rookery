{
  description = "A rookery of jj workspaces for coding agents";

  inputs = {
    # 26.05: the channel the project is developed against. Consumers that pin
    # this flake with `inputs.nixpkgs.follows = "nixpkgs"` build the package
    # with their own jj, so the CLI the tool invokes matches the repository
    # it operates on.
    nixpkgs.url = "github:nixos/nixpkgs/nixos-26.05";
    flake-parts.url = "github:hercules-ci/flake-parts";
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs = inputs:
    inputs.flake-parts.lib.mkFlake {inherit inputs;} {
      # The tool reads /proc and drives systemd user units through the module.
      systems = ["x86_64-linux"];

      imports = [inputs.treefmt-nix.flakeModule];

      flake.homeManagerModules.default = import ./nix/module.nix inputs;

      perSystem = {
        config,
        pkgs,
        ...
      }: let
        rookery = pkgs.callPackage ./packages/rookery.nix {};
      in {
        packages.rookery = rookery;
        packages.default = rookery;

        # The board's classification, cache key and freshness rules against a
        # fixture repository: cold/warm/serial/parallel output equality,
        # invalidation on describe, push, fetch and workspace add, and live,
        # session and wait state that must never come from the cache.
        checks.rookery-board =
          pkgs.runCommand "rookery-board-test" {
            nativeBuildInputs = [
              rookery
              pkgs.jujutsu
              pkgs.git
              pkgs.sqlite
            ];
          } ''
            cp ${./tests/board.sh} board.sh
            chmod +x board.sh
            bash board.sh
            touch $out
          '';

        devShells.default = pkgs.mkShell {
          inputsFrom = [config.treefmt.build.devShell];
          packages = [
            rookery
            pkgs.jujutsu
            pkgs.git
            pkgs.sqlite
            pkgs.shellcheck
            pkgs.prek
          ];
          name = "rookery";
        };

        treefmt.config = {
          projectRootFile = "flake.nix";
          programs = {
            alejandra.enable = true;
            shfmt.enable = true;
            prettier.enable = true;
          };
          settings.global.excludes = [
            ".direnv/*"
            "result"
            "result-*"
          ];
        };
      };
    };
}
