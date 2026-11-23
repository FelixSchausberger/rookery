# Home Manager module for ocws.
#
# The package reads its configuration from the environment at runtime, so the
# module wraps the binary with the option values as defaults. A runtime
# override still wins, which keeps `OCWS_BASE=... ocws ls` and the fixture
# tests working unchanged.
inputs: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.programs.ocws;

  wrapperArgs =
    [
      "--set-default"
      "OCWS_WS_PREFIX"
      cfg.workspacePrefix
      "--set-default"
      "OCWS_TRUNK"
      cfg.trunk
      "--set-default"
      "OCWS_GC_AGE"
      (toString cfg.gc.age)
      "--set-default"
      "OCWS_GC_EMPTY_AGE"
      (toString cfg.gc.emptyAge)
      "--set-default"
      "OCWS_GC_SESSION_AGE"
      (toString cfg.gc.sessionAge)
      "--set-default"
      "OCWS_WAIT_STALE_DAYS"
      (toString cfg.waitStaleDays)
      "--set-default"
      "OCWS_HOOK_PRE_CREATE"
      cfg.hooks.preCreate
      "--set-default"
      "OCWS_HOOK_POST_CREATE"
      cfg.hooks.postCreate
      "--set-default"
      "OCWS_HOOK_PRE_DELETE"
      cfg.hooks.preDelete
      "--set-default"
      "OCWS_HOOK_POST_DELETE"
      cfg.hooks.postDelete
    ]
    ++ lib.optionals (cfg.base != null) ["--set-default" "OCWS_BASE" cfg.base];

  finalPackage =
    pkgs.runCommand "ocws-configured" {
      nativeBuildInputs = [pkgs.makeWrapper];
    } ''
      mkdir -p $out/bin
      makeWrapper ${cfg.package}/bin/ocws $out/bin/ocws ${lib.escapeShellArgs wrapperArgs}
    '';
in {
  options.programs.ocws = {
    enable = lib.mkEnableOption "the ocws jj workspace seat manager";

    package = lib.mkOption {
      type = lib.types.package;
      default = pkgs.callPackage "${inputs.self}/packages/ocws.nix" {};
      defaultText = lib.literalExpression "the ocws package from this flake";
      description = "The ocws package to wrap with the options below.";
    };

    base = lib.mkOption {
      type = lib.types.nullOr lib.types.str;
      default = null;
      example = "~/.local/share/ocws";
      description = ''
        Directory holding the workspace directories. The default follows the
        script's precedence: the parent of the current workspace when writable,
        then `$XDG_DATA_HOME/ocws`, then `/tmp/opencode`.
      '';
    };

    workspacePrefix = lib.mkOption {
      type = lib.types.str;
      default = "ws-";
      description = "Prefix of a workspace directory name, before the task name.";
    };

    trunk = lib.mkOption {
      type = lib.types.str;
      default = "main@origin";
      example = "trunk@origin";
      description = ''
        Bookmark the workspace base is taken from and the states are classified
        against. It must exist after `jj git fetch`.
      '';
    };

    waitStaleDays = lib.mkOption {
      type = lib.types.ints.positive;
      default = 14;
      description = "Days after which a parked workspace resurfaces as a gc nudge.";
    };

    hooks = {
      preCreate = lib.mkOption {
        type = lib.types.str;
        default = "";
        description = "Shell command run before a workspace is created.";
      };
      postCreate = lib.mkOption {
        type = lib.types.str;
        default = "";
        description = "Shell command run after a workspace is created.";
      };
      preDelete = lib.mkOption {
        type = lib.types.str;
        default = "";
        description = "Shell command run before a workspace is removed.";
      };
      postDelete = lib.mkOption {
        type = lib.types.str;
        default = "";
        description = "Shell command run after a workspace is removed.";
      };
    };

    gc = {
      enable = lib.mkEnableOption "the daily ocws GC user timer";

      repo = lib.mkOption {
        type = lib.types.nullOr lib.types.str;
        default = null;
        example = "/home/user/src/repo";
        description = ''
          Repository the GC pass runs in. The service exits cleanly when the
          directory is absent, so a host whose checkout is optional stays
          green.
        '';
      };

      onCalendar = lib.mkOption {
        type = lib.types.str;
        default = "daily";
        description = "systemd calendar expression for the GC timer.";
      };

      persistent = lib.mkOption {
        type = lib.types.bool;
        default = true;
        description = "Run a missed GC pass when the timer was not active.";
      };

      age = lib.mkOption {
        type = lib.types.ints.positive;
        default = 86400;
        description = "Seconds a workspace that held work must sit idle before gc removes it.";
      };

      emptyAge = lib.mkOption {
        type = lib.types.ints.positive;
        default = 21600;
        description = "Seconds an empty, unclaimed workspace must sit idle before gc removes it.";
      };

      sessionAge = lib.mkOption {
        type = lib.types.ints.positive;
        default = 604800;
        description = "Seconds a recent agent session keeps its workspace out of gc.";
      };
    };

    finalPackage = lib.mkOption {
      type = lib.types.package;
      readOnly = true;
      description = ''
        The wrapped package. Consumers that invoke ocws by absolute path (a
        terminal keybinding, a systemd unit) must use this, not `package`.
      '';
    };
  };

  config = lib.mkIf cfg.enable {
    programs.ocws.finalPackage = finalPackage;

    home.packages = [cfg.finalPackage];

    assertions = [
      {
        assertion = !cfg.gc.enable || cfg.gc.repo != null;
        message = "programs.ocws.gc.repo must be set when programs.ocws.gc.enable is true";
      }
    ];

    # The guard lives in the script, not in WorkingDirectory: a consumer host
    # that imports the module without the checkout must exit 0, while systemd
    # fails the unit before ExecStart when a WorkingDirectory is missing.
    systemd.user.services.ocws-gc = lib.mkIf (cfg.gc.enable && cfg.gc.repo != null) {
      Unit.Description = "Reclaim finished jj workspaces";
      Service = {
        Type = "oneshot";
        ExecStart = toString (pkgs.writeShellScript "ocws-gc" ''
          set -euo pipefail
          repo=${lib.escapeShellArg cfg.gc.repo}
          [ -d "$repo" ] || exit 0
          cd "$repo"
          ${cfg.finalPackage}/bin/ocws gc
        '');
      };
    };

    systemd.user.timers.ocws-gc = lib.mkIf (cfg.gc.enable && cfg.gc.repo != null) {
      Install.WantedBy = ["timers.target"];
      Timer = {
        OnCalendar = cfg.gc.onCalendar;
        Persistent = cfg.gc.persistent;
      };
    };
  };
}
