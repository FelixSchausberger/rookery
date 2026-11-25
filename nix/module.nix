# Home Manager module for Rookery.
#
# The package reads its configuration from the environment at runtime, so the
# module wraps the binary with the option values as defaults. A runtime
# ROOKERY_* variable still wins; the pre-rename OCWS_* names are read as
# fallbacks when the new name is unset.
inputs: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.programs.rookery;

  wrapperArgs =
    [
      "--set-default"
      "ROOKERY_WS_PREFIX"
      cfg.workspacePrefix
      "--set-default"
      "ROOKERY_TRUNK"
      cfg.trunk
      "--set-default"
      "ROOKERY_GC_AGE"
      (toString cfg.gc.age)
      "--set-default"
      "ROOKERY_GC_EMPTY_AGE"
      (toString cfg.gc.emptyAge)
      "--set-default"
      "ROOKERY_GC_SESSION_AGE"
      (toString cfg.gc.sessionAge)
      "--set-default"
      "ROOKERY_REAP_CHILD_AGE"
      (toString cfg.gc.reapChildAge)
      "--set-default"
      "ROOKERY_WAIT_STALE_DAYS"
      (toString cfg.waitStaleDays)
      "--set-default"
      "ROOKERY_HOOK_PRE_CREATE"
      cfg.hooks.preCreate
      "--set-default"
      "ROOKERY_HOOK_POST_CREATE"
      cfg.hooks.postCreate
      "--set-default"
      "ROOKERY_HOOK_PRE_DELETE"
      cfg.hooks.preDelete
      "--set-default"
      "ROOKERY_HOOK_POST_DELETE"
      cfg.hooks.postDelete
    ]
    ++ lib.optionals (cfg.base != null) ["--set-default" "ROOKERY_BASE" cfg.base];

  finalPackage =
    pkgs.runCommand "rookery-configured" {
      nativeBuildInputs = [pkgs.makeWrapper];
    } ''
      mkdir -p $out/bin
      makeWrapper ${cfg.package}/bin/rookery $out/bin/rookery ${lib.escapeShellArgs wrapperArgs}
      ln -s rookery $out/bin/ocws
    '';

  # The board reads the same environment defaults as the CLI, and the pane a
  # seat opens re-enters the configured CLI through ROOKERY_BIN.
  finalTuiPackage =
    pkgs.runCommand "rookery-tui-configured" {
      nativeBuildInputs = [pkgs.makeWrapper];
    } ''
      mkdir -p $out/bin
      makeWrapper ${cfg.tui.package}/bin/rookery-tui $out/bin/rookery-tui \
        ${lib.escapeShellArgs (wrapperArgs ++ ["--set-default" "ROOKERY_BIN" "${finalPackage}/bin/rookery"])}
    '';
in {
  options.programs.rookery = {
    enable = lib.mkEnableOption "the Rookery jj workspace manager";

    package = lib.mkOption {
      type = lib.types.package;
      default = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.rookery;
      defaultText = lib.literalExpression "the Rust Rookery package from this flake";
      description = "The Rookery package to wrap with the options below. The legacy shell command remains available as `rookery-legacy`.";
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
      enable = lib.mkEnableOption "the daily rookery GC user timer";

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

      reapChildAge = lib.mkOption {
        type = lib.types.ints.positive;
        default = 604800;
        description = ''
          Seconds a child subagent transcript must sit idle before `reap`
          prunes it. Top-level conversations are never pruned by the default
          pass; only `reap --roots` deletes one whose workspace is gone.
        '';
      };
    };

    finalPackage = lib.mkOption {
      type = lib.types.package;
      readOnly = true;
      description = ''
        The wrapped package. Consumers that invoke rookery by absolute path (a
        terminal keybinding, a systemd unit) must use this, not `package`.
      '';
    };

    tui = {
      enable = lib.mkEnableOption "the Rookery board TUI";

      package = lib.mkOption {
        type = lib.types.package;
        default = inputs.self.packages.${pkgs.stdenv.hostPlatform.system}.rookery-tui;
        defaultText = lib.literalExpression "the rookery-tui package from this flake";
        description = "The board TUI binary to install.";
      };

      finalPackage = lib.mkOption {
        type = lib.types.package;
        readOnly = true;
        description = ''
          The board wrapped with the module's environment defaults. A
          keybinding or popup should run this, not `package`.
        '';
      };
    };
  };

  config = lib.mkIf cfg.enable {
    programs.rookery.finalPackage = finalPackage;
    programs.rookery.tui.finalPackage = finalTuiPackage;

    home.packages =
      [cfg.finalPackage]
      ++ lib.optional cfg.tui.enable cfg.tui.finalPackage;

    assertions = [
      {
        assertion = !cfg.gc.enable || cfg.gc.repo != null;
        message = "programs.rookery.gc.repo must be set when programs.rookery.gc.enable is true";
      }
    ];

    # The guard lives in the script, not in WorkingDirectory: a consumer host
    # that imports the module without the checkout must exit 0, while systemd
    # fails the unit before ExecStart when a WorkingDirectory is missing.
    systemd.user.services.rookery-gc = lib.mkIf (cfg.gc.enable && cfg.gc.repo != null) {
      Unit.Description = "Reclaim finished jj workspaces";
      Service = {
        Type = "oneshot";
        ExecStart = toString (pkgs.writeShellScript "rookery-gc" ''
          set -euo pipefail
          repo=${lib.escapeShellArg cfg.gc.repo}
          [ -d "$repo" ] || exit 0
          cd "$repo"
          ${cfg.finalPackage}/bin/rookery gc
          # gc removes the stale directories; the reaper prunes child
          # subagent transcripts and keeps every top-level conversation.
          ${cfg.finalPackage}/bin/rookery reap
        '');
      };
    };

    systemd.user.timers.rookery-gc = lib.mkIf (cfg.gc.enable && cfg.gc.repo != null) {
      Install.WantedBy = ["timers.target"];
      Timer = {
        OnCalendar = cfg.gc.onCalendar;
        Persistent = cfg.gc.persistent;
      };
    };
  };
}
