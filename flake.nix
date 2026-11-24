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
        rookeryTui = pkgs.rustPlatform.buildRustPackage {
          pname = "rookery-tui";
          version = "0.4.0";
          src = pkgs.lib.cleanSourceWith {
            src = ./tui;
            filter = path: _: let
              base = baseNameOf path;
            in
              base != "target" && base != "result";
          };
          cargoLock.lockFile = ./tui/Cargo.lock;
          meta = {
            description = "The Rookery board: tasks and their conversations";
            homepage = "https://github.com/FelixSchausberger/rookery";
            license = pkgs.lib.licenses.mit;
            mainProgram = "rookery-tui";
          };
        };
      in {
        packages.rookery = rookery;
        packages.rookery-tui = rookeryTui;
        packages.default = rookery;

        # The TUI is a client of the CLI contract; the check builds the crate
        # and runs its tests (lane derivation, key handling, CLI argv, and the
        # TestBackend snapshots) in the sandbox.
        checks.rookery-tui = rookeryTui;

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

        # The task layer against a fixture repository and a fake herdr: NDJSON
        # board, store writes and maintenance across rename/removal, cleanup
        # marks and the sweep, seat focus and task seats, and corruption
        # handling. Nothing touches the real base or cache.
        checks.rookery-tasks =
          pkgs.runCommand "rookery-tasks-test" {
            nativeBuildInputs = [
              rookery
              pkgs.jujutsu
              pkgs.git
              pkgs.sqlite
              pkgs.jq
              pkgs.util-linux
              pkgs.gnugrep
              pkgs.gnused
              pkgs.coreutils
              pkgs.bash
            ];
          } ''
            cp ${./tests/tasks.sh} tasks.sh
            chmod +x tasks.sh
            bash tasks.sh
            touch $out
          '';

        devShells.default = pkgs.mkShell {
          inputsFrom = [config.treefmt.build.devShell];
          packages = [
            rookery
            pkgs.jujutsu
            pkgs.git
            pkgs.sqlite
            pkgs.jq
            pkgs.shellcheck
            pkgs.prek
            pkgs.cargo
            pkgs.rustc
            pkgs.rustfmt
            pkgs.clippy
          ];
          name = "rookery";
        };

        treefmt.config = {
          projectRootFile = "flake.nix";
          programs = {
            alejandra.enable = true;
            shfmt.enable = true;
            prettier.enable = true;
            rustfmt.enable = true;
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
