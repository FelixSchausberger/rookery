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
        rookeryLegacy = pkgs.callPackage ./packages/rookery.nix {};
        workspaceToml = builtins.fromTOML (builtins.readFile ./Cargo.toml);
        runtimeDeps = with pkgs; [
          jujutsu
          coreutils
          direnv
          gnugrep
          gnused
          findutils
          rip2
          zellij
          jq
          sqlite
          util-linux
          bash
        ];
        testDeps = [
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
        workspaceSrc = pkgs.lib.cleanSourceWith {
          src = ./.;
          filter = path: _: let
            base = baseNameOf path;
          in
            !(builtins.elem base ["target" "result" ".direnv" ".jj"])
            && !(pkgs.lib.hasPrefix "result-" base);
        };
        rookeryRs = pkgs.rustPlatform.buildRustPackage {
          pname = "rookery";
          version = workspaceToml.workspace.package.version;
          src = workspaceSrc;
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = ["--package" "rookery-cli" "--bin" "rookery"];
          cargoTestFlags = ["--package" "rookery-core" "--package" "rookery-cli"];
          buildInputs = [pkgs.sqlite];
          nativeBuildInputs = [pkgs.makeWrapper pkgs.pkg-config];
          postInstall = ''
            wrapProgram $out/bin/rookery --prefix PATH : ${pkgs.lib.makeBinPath runtimeDeps}
            ln -s rookery $out/bin/ocws
          '';
          meta = {
            description = "A rookery of jj workspaces for coding agents";
            homepage = "https://github.com/FelixSchausberger/rookery";
            license = pkgs.lib.licenses.mit;
            mainProgram = "rookery";
          };
        };
        rookeryTui = pkgs.rustPlatform.buildRustPackage {
          pname = "rookery-tui";
          version = workspaceToml.workspace.package.version;
          src = workspaceSrc;
          cargoLock.lockFile = ./Cargo.lock;
          cargoBuildFlags = ["--package" "rookery-tui" "--bin" "rookery-tui"];
          cargoTestFlags = ["--package" "rookery-tui"];
          buildInputs = [pkgs.sqlite];
          nativeBuildInputs = [pkgs.makeWrapper pkgs.pkg-config];
          postInstall = ''
            wrapProgram $out/bin/rookery-tui --prefix PATH : ${pkgs.lib.makeBinPath runtimeDeps}
          '';
          meta = {
            description = "The Rookery board: tasks and their conversations";
            homepage = "https://github.com/FelixSchausberger/rookery";
            license = pkgs.lib.licenses.mit;
            mainProgram = "rookery-tui";
          };
        };
      in {
        packages.rookery = rookeryRs;
        packages.rookery-legacy = rookeryLegacy;
        packages.rookery-tui = rookeryTui;
        packages.default = rookeryRs;

        # The TUI links rookery-core; the check builds the workspace member and
        # runs its key-handling and TestBackend snapshot tests in the sandbox.
        checks.rookery-tui = rookeryTui;

        # The board's classification, cache key and freshness rules against a
        # fixture repository: cold/warm/serial/parallel output equality,
        # invalidation on describe, push, fetch and workspace add, and live,
        # session and wait state that must never come from the cache.
        checks.rookery-board =
          pkgs.runCommand "rookery-board-test" {
            nativeBuildInputs = [rookeryRs] ++ testDeps;
          } ''
            cp ${./tests/board.sh} board.sh
            chmod +x board.sh
            bash board.sh
            touch $out
          '';

        checks.rookery-tasks =
          pkgs.runCommand "rookery-tasks-test" {
            nativeBuildInputs = [rookeryRs] ++ testDeps;
          } ''
            cp ${./tests/tasks.sh} tasks.sh
            chmod +x tasks.sh
            bash tasks.sh
            touch $out
          '';

        checks.rookery-parity =
          pkgs.runCommand "rookery-parity-test" {
            nativeBuildInputs = [rookeryLegacy rookeryRs] ++ testDeps;
          } ''
            cp ${./tests/parity.sh} parity.sh
            chmod +x parity.sh
            ROOKERY_ORACLE=${rookeryLegacy}/bin/rookery \
              ROOKERY_CANDIDATE=${rookeryRs}/bin/rookery \
              bash parity.sh
            touch $out
          '';

        devShells.default = pkgs.mkShell {
          inputsFrom = [config.treefmt.build.devShell];
          packages = [
            rookeryRs
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
