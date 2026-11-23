# rookery - agent workspace manager. Creates a jj workspace for a single agent,
# claims it with a change description, and starts an agent rooted there, then
# reclaims finished workspaces. jj workspaces are the shared-repo equivalent
# of git worktrees and are the supported way to run parallel agents against
# one repository.
#
# `rookery ls` classifies every workspace (empty, merged, unpushed, in-flight,
# conflict) from the same absorption test that drives reclamation, so stale
# workspaces are visible without mutating anything. `rookery gc` reclaims the
# empty and merged ones; the Home Manager module ships a systemd user timer.
#
# A workspace can be parked: `rookery wait <task> [reason...]` marks it waiting
# through an <task>.wait marker beside the workspace directory, and gc keeps
# it whatever its commit state, because a session waiting on the outside
# world (an upstream fix, a device) is not reclaimable regardless of what jj
# sees. Waits older than ROOKERY_WAIT_STALE_DAYS resurface as gc nudges, so a
# forgotten park is visible instead of a permanent free pass.
#
# Tasks group workspaces under a stable id in <base>/tasks.json. The grouping
# is the only stored board state; a task's lane is derived from its
# workspaces. `rookery ls --json` and `rookery task list --json` expose the
# board as NDJSON for a TUI client, and every write goes through the CLI.
# `rookery clean` marks a finished workspace with an <workspace>.clean marker;
# `clean --apply` removes the marked ones that are merged or empty, idle and
# free of unpushed work. A clean mark is a person's request, not a state, so it
# never appears as a lane in the store.
#
# Liveness ignores per-project MCP and LSP daemons: they inherit the cwd of
# the session that spawned them and outlive it, so a cwd-only occupancy check
# holds workspaces hostage after the session closed. A cwd-only check also
# misses a session whose TUI already exited, so gc consults the agent's
# session database: an unarchived session rooted in the workspace and updated
# within ROOKERY_GC_SESSION_AGE keeps the workspace with no process at all.
# Recency, not a pending prompt, is the observable state - unanswered
# permission asks live in the server's memory and vanish with the client - so
# this is a grace window; 'rookery wait' remains the indefinite marker. The probe
# is guard-silent: no database, no sqlite3, or an unreadable schema falls back
# to the process and touch checks.
#
# Base directory precedence: $ROOKERY_BASE, then the parent directory of the
# current workspace (the sibling convention) when writable, then
# $XDG_DATA_HOME/ocws (default ~/.local/share/ocws) so workspaces survive
# reboots without ever needing root, then /tmp/opencode. The workspace
# directory is "<base>/<ROOKERY_WS_PREFIX><task>"; the prefix defaults to "ws-".
#
# The trunk defaults to main@origin and can be overridden with ROOKERY_TRUNK, for
# repositories whose trunk is named or tracked differently. Classification,
# absorption, gc and the base revision of a new workspace all use it.
#
# ROOKERY_V2=1 runs the opencode2 entry point instead of opencode. Both names
# resolve to the same launcher.
#
# ROOKERY_HOOK_{PRE,POST}_{CREATE,DELETE} run a shell command around workspace
# creation and removal for per-workspace setup/teardown (dev ports, .env
# files, caches). ROOKERY_TASK/DIR/ROOT/BASE are exported to the hook.
{
  lib,
  writeShellApplication,
  jujutsu,
  coreutils,
  direnv,
  gnugrep,
  gnused,
  findutils,
  rip2,
  zellij,
  jq,
  sqlite,
  util-linux,
  bash,
  symlinkJoin,
}: let
  command = writeShellApplication {
    name = "rookery";
    # Every single-quoted $variable in this script is a jq program reading a
    # --arg value, not a shell expansion. SC2016 would flag each one.
    excludeShellChecks = ["SC2016"];
    runtimeInputs = [
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
    text = ''
      set -euo pipefail

      # Accept the pre-rename OCWS_* names for one release. The new name wins
      # when both are set. The wrapper supplies defaults under the new names, so
      # an override of a configured option uses ROOKERY_*, not OCWS_*.
      for _name in BASE WS_PREFIX TRUNK V2 GC_AGE GC_EMPTY_AGE GC_SESSION_AGE \
        WAIT_STALE_DAYS LIVE_EXCLUDE BOARD_JOBS NO_MAINT \
        HOOK_PRE_CREATE HOOK_POST_CREATE HOOK_PRE_DELETE HOOK_POST_DELETE; do
        _old="OCWS_$_name"
        _new="ROOKERY_$_name"
        if [ -z "''${!_new:-}" ] && [ -n "''${!_old:-}" ]; then
          export "$_new=''${!_old}"
        fi
      done
      unset _name _old _new

      usage() {
        cat >&2 <<'USAGE'
      Usage:
        rookery <task> [-- <opencode args>...]   create a workspace and run opencode there
        rookery tab <task> [-- <opencode args>]  create a workspace in a new zellij tab
        rookery herdr [--json] [--no-focus] [--workspace ID] [--wait[=SECONDS]] [--task ID] [<task>] [-- <opencode args>]
                                                 start an agent seat: create the workspace
                                                 and run opencode in a new herdr tab; an
                                                 unnamed seat gets a filler task that
                                                 'rookery title' replaces. --json prints the
                                                 tab, pane and workspace for a lead;
                                                 --no-focus keeps the person's view still;
                                                 --wait blocks until the agent is at its
                                                 prompt (180s by default); --task ID
                                                 assigns the new seat to an existing task
        rookery seat [--json]                    print the calling agent's herdr workspace,
                                                 tab and pane, matched by OPENCODE_SESSION_ID
        rookery title <text...>                  claim the current seat: set the change
                                                 description, rename the workspace and the
                                                 herdr tab from the session title; a no-op
                                                 unless the description is still the filler
        rookery ls [--counts|--board|--json] [--refresh]
                                                 list workspaces with their state and claim
                                                 (--counts prints the integers "total empty merged
                                                 unpushed in-flight conflict stale waitold", where
                                                 waitold counts parked workspaces older than
                                                 ROOKERY_WAIT_STALE_DAYS; --board groups the same
                                                 classification into "what needs you" sections;
                                                 --json prints one JSON object per workspace for a
                                                 board client; --refresh ignores the classification
                                                 cache and scans now, rewriting it)
        rookery task add <title...> [--id ID] [--notes TEXT]
                                                 create a backlog task; the id derives from the
                                                 title unless --id is given
        rookery task assign [--move] <task> <workspace>
                                                 group an existing workspace under a task
        rookery task list [--json]               list tasks with their workspaces
        rookery task done <task>                 mark every workspace of the task for cleanup
        rookery task archive <task>              archive a task whose workspaces are gone
        rookery clean [--current|<workspace>]    mark a workspace for the safe cleanup sweep
        rookery clean --unmark [--current|<workspace>]
                                                 clear the cleanup mark
        rookery clean --apply                    remove marked workspaces that are merged or
                                                 empty, idle and free of unpushed work
        rookery open <workspace|task>            focus a workspace's seat (herdr today)
        rookery wait <task> [reason...]          park a workspace: gc keeps it until 'rookery go'
                                                 clears the marker, whatever its commit state -
                                                 for a session waiting on an external blocker
                                                 (an upstream fix, a device) that must not hold
                                                 its pane open
        rookery go <task>                        release a parked workspace (removes its wait
                                                 marker); the next gc pass reclassifies it
        rookery done [--force] <task>            remove a finished workspace (refuses unmerged work
                                                 or a live process; --force overrides both)
        rookery rm [--force] <task>              remove a workspace; --force discards unmerged work
        rookery gc                               remove reclaimable workspaces idle past the age gate

      Environment:
        ROOKERY_BASE
          directory holding workspace directories (default: beside the repo
          when writable, else ~/.local/share/ocws, else /tmp/opencode)
        ROOKERY_WS_PREFIX
          prefix of a workspace directory name, before the task (default: ws-)
        ROOKERY_TRUNK
          the bookmark the workspace base is taken from and the states are
          classified against (default: main@origin)
        ROOKERY_V2=1
          run the opencode2 entry point instead of opencode
        ROOKERY_GC_AGE
          seconds a workspace that had work must sit idle before gc removes it
          (default 86400)
        ROOKERY_GC_EMPTY_AGE
          seconds an empty, unclaimed workspace must sit idle before gc removes
          it (default 21600)
        ROOKERY_GC_SESSION_AGE
          seconds an opencode session rooted in a workspace keeps that workspace
          out of gc, measured from the session's last update (default 604800)
        ROOKERY_WAIT_STALE_DAYS
          days after which a parked workspace resurfaces as a gc nudge and in
          the prompt badge (default 14)
        ROOKERY_LIVE_EXCLUDE
          grep -E pattern additionally excluded from the liveness scan, for
          detecting future auxiliary processes (per-project MCP or LSP daemons)
          that inherit their parent's cwd
        ROOKERY_BOARD_JOBS
          parallel workers for the board's absorption checks and its
          process/session probes (default 8, clamped to 1-32)
        ROOKERY_HOOK_{PRE,POST}_{CREATE,DELETE}
          optional shell commands run around workspace create/remove, with
          ROOKERY_TASK/DIR/ROOT/BASE exported (the OCWS_* spellings too)
      USAGE
        exit 2
      }

      root="$(jj workspace root 2>/dev/null)" || {
        echo "rookery: not inside a jj workspace" >&2
        exit 1
      }

      trunk="''${ROOKERY_TRUNK:-main@origin}"
      ws_prefix="''${ROOKERY_WS_PREFIX:-ws-}"

      base="''${ROOKERY_BASE:-}"
      if [ -z "$base" ]; then
        candidate="$(dirname "$root")"
        if [ -w "$candidate" ]; then
          base="$candidate"
        else
          base="''${XDG_DATA_HOME:-$HOME/.local/share}/ocws"
          mkdir -p "$base"
        fi
      fi
      if [ ! -w "$base" ]; then
        base=/tmp/opencode
      fi
      mkdir -p "$base"

      valid_task() { printf '%s' "$1" | grep -Eq '^[A-Za-z0-9][A-Za-z0-9._-]*$'; }
      ws_dir_for() { printf '%s/%s%s' "$base" "$ws_prefix" "$1"; }
      # Wait markers live beside the workspace directories, never inside them: a
      # marker under the working copy would be snapshotted into the next claim
      # commit by any session that happens to run there. Keyed by task name, and
      # rip_workspace removes it with the workspace so clearing cannot be
      # forgotten by any of the removal paths (done, rm, gc).
      wait_marker_for() { printf '%s/%s.wait' "$base" "$1"; }
      # ROOKERY_WAIT_STALE_DAYS must survive invalid input without killing the
      # script under errexit: arithmetic on a non-integer aborts a gc pass.
      # The digit check is grep-based, not a shell case on an empty pattern:
      # contiguous quote pairs inside this derivation body terminate the nix
      # indented string, so the shell must never carry two adjacent quotes here.
      wait_stale_days() {
        v="''${ROOKERY_WAIT_STALE_DAYS:-14}"
        if printf '%s' "$v" | grep -Eq '^[0-9]+$'; then
          printf '%s' "$v"
        else
          echo 14
        fi
      }
      # Workspace listings and the finished-work check are read-only and must
      # never snapshot a foreign working copy: an `rookery ls` run from the primary
      # or another session's workspace would otherwise fold that workspace's
      # uncommitted changes into its @.
      workspace_exists() {
        local names
        names="$(jj --ignore-working-copy workspace list -T 'name ++ "\n"' 2>/dev/null)"
        grep -qx "$1" <<<"$names"
      }
      # Resolve a workspace directory from jj's recorded root (jj >= 0.38 prints
      # an absolute path), falling back to the base-directory convention for
      # pre-0.38 workspaces whose root is not recorded.
      ws_root_for() {
        local out
        out="$(jj --ignore-working-copy workspace list -T 'name ++ "\t" ++ root ++ "\n"' 2>/dev/null | sed -n "s/^$1\t//p")"
        if [ -n "$out" ] && [ "$out" != "none" ]; then
          printf '%s' "$out"
        else
          ws_dir_for "$1"
        fi
      }
      # Clean markers live beside the wait markers, keyed by workspace name.
      # `clean --apply` is the only reader, and every removal path clears the
      # mark through rip_workspace, so a mark cannot outlive its workspace.
      clean_marker_for() { printf '%s/%s.clean' "$base" "$1"; }
      # Task ids and workspace names share the alphabet, so one validator
      # covers both. Titles become ids through the same slug rules `title`
      # uses for a claimed workspace.
      task_slug() {
        printf '%s' "$1" \
          | tr '[:upper:]' '[:lower:]' \
          | tr -cs '[:alnum:]' '-' \
          | sed -e 's/^-*//' -e 's/-*$//' \
          | cut -c1-48 \
          | sed -e 's/-*$//'
      }
      # The workspace whose recorded root contains the current directory. A
      # prefix match, so a command run from a subdirectory still resolves.
      workspace_at_cwd() {
        local here name ws_root
        here="$(realpath -m "$PWD")"
        while IFS=$'\t' read -r name ws_root; do
          case "$here/" in
            "$(realpath -m "$ws_root")/"*)
              printf '%s' "$name"
              return 0
              ;;
          esac
        done < <(jj --ignore-working-copy workspace list -T 'name ++ "\t" ++ root ++ "\n"' 2>/dev/null)
        return 1
      }
      # --- task store -------------------------------------------------------
      # The grouping lives in one JSON document beside the workspaces. Every
      # write goes through store_mutate: flock on a sidecar file (the store
      # itself is replaced by rename, so locking its inode would lock a file
      # nobody reads), jq into a temp file in the same directory, rename. A
      # reader sees the old or the new document, never a partial write, so read
      # paths take no lock. A missing store is an empty document; a corrupt one
      # is reported by the write path instead of being overwritten.
      task_store() { printf '%s/tasks.json' "$base"; }
      task_store_lock() { printf '%s/tasks.json.lock' "$base"; }
      store_read() {
        local file
        file="$(task_store)"
        if [ -f "$file" ]; then
          cat "$file"
        else
          printf '{"version":1,"tasks":[]}\n'
        fi
      }
      store_query() {
        local filter="$1"
        shift
        store_read | jq -r "$@" "$filter" 2>/dev/null
      }
      store_task_exists() {
        [ "$(store_query 'any(.tasks[]; .id == $id)' --arg id "$1")" = "true" ]
      }
      store_task_open() {
        [ "$(store_query 'any(.tasks[]; .id == $id and .status == "open")' --arg id "$1")" = "true" ]
      }
      store_ws_task() {
        store_query 'first(.tasks[] | select(.workspaces | index($ws)) | .id) // ""' --arg ws "$1"
      }
      # A corrupt store must be reported by the commands that own it, not
      # silently rewritten. The board tolerates it and shows unassigned rows.
      store_check() {
        local file body
        file="$(task_store)"
        [ -f "$file" ] || return 0
        body="$(cat "$file")" || return 1
        printf '%s' "$body" | jq -e 'has("tasks") and (.tasks | type == "array")' >/dev/null 2>&1 || {
          echo "rookery: task store is unreadable: $file" >&2
          return 1
        }
      }
      declare -A task_of_ws=()
      store_ws_map() {
        local ws id
        task_of_ws=()
        while IFS=$'\t' read -r ws id; do
          [ -n "$ws" ] && task_of_ws["$ws"]="$id"
        done < <(store_query '.tasks[] | .id as $id | .workspaces[] | [., $id] | @tsv' 2>/dev/null || true)
      }
      # Mutations return 0 and the store is replaced atomically; a filter refusal
      # (a duplicate id, a workspace owned by another task) returns 2 with the
      # message on stderr, and the store is left untouched.
      store_mutate() {
        local filter="$1"
        shift
        local file lock tmp fd out err
        file="$(task_store)"
        lock="$(task_store_lock)"
        mkdir -p "$base"
        exec {fd}>"$lock" 2>/dev/null || {
          echo "rookery: cannot open the task store lock" >&2
          return 1
        }
        if ! flock -w 10 "$fd"; then
          exec {fd}>&- 2>/dev/null || true
          echo "rookery: task store is busy; retry" >&2
          return 1
        fi
        if [ ! -f "$file" ]; then
          printf '{"version":1,"tasks":[]}\n' >"$file" || {
            exec {fd}>&- 2>/dev/null || true
            echo "rookery: cannot create the task store" >&2
            return 1
          }
        fi
        tmp="$(mktemp "$base/.tasks.XXXXXX")" || {
          exec {fd}>&- 2>/dev/null || true
          echo "rookery: cannot write the task store" >&2
          return 1
        }
        if ! out="$(jq --argjson now "$(date +%s)" "$@" "$filter" "$file" 2>"$tmp.err")"; then
          err="$(sed -n '1p' "$tmp.err")"
          rm -f "$tmp" "$tmp.err"
          exec {fd}>&- 2>/dev/null || true
          echo "rookery: task store: ''${err:-unreadable}" >&2
          return 1
        fi
        rm -f "$tmp.err"
        err="$(printf '%s' "$out" | jq -r 'select(has("error")) | .error' 2>/dev/null || true)"
        if [ -n "$err" ]; then
          rm -f "$tmp"
          exec {fd}>&- 2>/dev/null || true
          echo "rookery: $err" >&2
          return 2
        fi
        printf '%s\n' "$out" >"$tmp" || {
          rm -f "$tmp"
          exec {fd}>&- 2>/dev/null || true
          echo "rookery: cannot write the task store" >&2
          return 1
        }
        mv -f "$tmp" "$file" || {
          rm -f "$tmp"
          exec {fd}>&- 2>/dev/null || true
          echo "rookery: cannot replace the task store" >&2
          return 1
        }
        flock -u "$fd" 2>/dev/null || true
        exec {fd}>&- 2>/dev/null || true
      }
      store_add() {
        store_mutate '
          .tasks as $t
          | if ($t | any(.id == $id)) then
              {error: ("task " + $id + " already exists")}
            else
              .tasks += [{id: $id, title: $title, status: "open", workspaces: [], notes: $notes, created: $now, updated: $now}]
            end
        ' --arg id "$1" --arg title "$2" --arg notes "''${3:-}" >/dev/null
      }
      store_assign() {
        local move="false"
        [ "''${3:-0}" = "1" ] && move="true"
        store_mutate '
          .tasks as $t
          | ($t | map(select(.id == $id))) as $match
          | ($t | map(select(.workspaces | index($ws))) | (.[0].id // "")) as $owner
          | if ($match | length) == 0 then
              {error: ("no task named " + $id)}
            elif $match[0].status == "archived" then
              {error: ("task " + $id + " is archived")}
            elif $owner != "" and $owner != $id and ($move | not) then
              {error: ("workspace " + $ws + " belongs to task " + $owner + "; use --move")}
            else
              .tasks |= map(
                if .id == $id then
                  .workspaces = ((.workspaces - [$ws]) + [$ws]) | .updated = $now
                elif (.workspaces | index($ws)) then
                  .workspaces = (.workspaces - [$ws]) | .updated = $now
                else . end)
            end
        ' --arg id "$1" --arg ws "$2" --argjson move "$move" >/dev/null
      }
      store_rename_ws() {
        store_mutate '
          .tasks |= map(
            if (.workspaces | index($old)) then
              .workspaces = ((.workspaces - [$old]) + [$new]) | .updated = $now
            else . end)
        ' --arg old "$1" --arg new "$2" >/dev/null
      }
      store_drop_ws() {
        store_mutate '
          .tasks |= map(
            if (.workspaces | index($ws)) then
              .workspaces = (.workspaces - [$ws]) | .updated = $now
            else . end)
        ' --arg ws "$1" >/dev/null
      }
      store_archive() {
        store_mutate '
          .tasks as $t
          | ($t | map(select(.id == $id))) as $match
          | if ($match | length) == 0 then
              {error: ("no task named " + $id)}
            elif ($match[0].workspaces | length) > 0 then
              {error: ("task " + $id + " still has workspaces; run task done and clean --apply first")}
            else
              .tasks |= map(if .id == $id then (.status = "archived" | .updated = $now) else . end)
            end
        ' --arg id "$1" >/dev/null
      }
      # Pick the seat `open` focuses for a task: a live workspace when there is
      # one, otherwise the first grouped workspace that jj still lists.
      store_pick_workspace() {
        local id="$1" candidate chosen="" live_ws=""
        while IFS= read -r candidate; do
          [ -n "$candidate" ] || continue
          workspace_exists "$candidate" || continue
          [ -n "$chosen" ] || chosen="$candidate"
          if [ -z "$live_ws" ] && ws_live "$(realpath -m "$(ws_root_for "$candidate")")"; then
            live_ws="$candidate"
          fi
        done < <(store_query '.tasks[] | select(.id == $id) | .workspaces[]' --arg id "$id" 2>/dev/null || true)
        [ -n "$live_ws" ] && chosen="$live_ws"
        [ -n "$chosen" ] || return 1
        printf '%s' "$chosen"
      }
      # A revision is absorbed when every file it touches already has its content
      # on the trunk: a squash-merge replaces the draft commit but lands the
      # same bytes, so the draft is finished even though it is not an ancestor.
      # The touched files are batched into a single diff against the trunk
      # (empty exactly when every file matches) rather than one diff per file.
      # Pure jj, so it also works in a secondary workspace, which has no .git.
      # Exit 2 means the check itself failed (a jj error, or a commit that
      # vanished mid-scan): callers must treat that as work still present, never
      # as absorbed, or a transient failure would hide unpushed content.
      ws_commit_absorbed() {
        local cid="$1" out
        local -a files=()
        out="$(jj --ignore-working-copy diff -r "$cid" --name-only 2>/dev/null)" || return 2
        [ -n "$out" ] || return 0
        mapfile -t files <<<"$out"
        out="$(jj --ignore-working-copy diff --from "$trunk" --to "$cid" -- "''${files[@]}" 2>/dev/null)" || return 2
        [ -z "$out" ]
      }
      # Classification of one workspace from the commits reachable from its @
      # that carry content outside the trunk. The state is one of:
      #   empty     - nothing outside the trunk carries content
      #   merged    - off-trunk content exists but is fully absorbed into it
      #   unpushed  - off-trunk content not absorbed, no remote bookmark
      #   in-flight - off-trunk content not absorbed, a remote bookmark (open PR)
      #   conflict  - @ or an off-trunk commit carries a conflict
      # Absorption per revision is what lets a squash-merged PR count as finished:
      # it leaves the draft off the trunk, so ancestry alone would call it
      # unmerged. Only snapshotted commits are visible; a foreign workspace is
      # never snapshotted (that would fold a live session's edits into its @), so
      # unsnapshotted edits are invisible and rip's graveyard is the recovery net.
      #
      # A board scan computes absorption in a bounded parallel pool and points
      # absorb_dir at the per-commit results; gc and the removal paths leave it
      # empty and pay the checks inline. classify_ok turns false when an input
      # could not be read, so the board refuses to cache a guessed state.
      absorb_dir=""
      classify_ok=1
      absorbed_of() {
        local cid="$1" path
        if [ -n "$absorb_dir" ]; then
          path="$absorb_dir/absorbed-$cid"
          if [ -r "$path" ]; then
            printf '%s' "$(<"$path")"
          else
            printf '2'
          fi
          return 0
        fi
        ws_commit_absorbed "$cid" || {
          printf '%s' "$?"
          return 0
        }
        printf '0'
      }
      # Interpret one workspace's ancestry flag lines ("<commit_id> <flags>")
      # and set st_state. Flags carry C (conflict), E (empty), R (remote
      # bookmark) and M (contained in the trunk). Together the lines are the
      # revset "T | (ancestors(T) ~ ::trunk)": a trunk commit appears
      # only as the workspace target itself, which is why M skips content (an
      # edited trunk commit is not off-trunk work) but stops the ancestry walk.
      state_from_flags() {
        local cid flags has_work=0 has_remote=0 offmain=0 stat
        st_conflict=0
        while read -r cid flags; do
          [ -n "$cid" ] || continue
          case "$flags" in *C*) st_conflict=1 ;; esac
          case "$flags" in *M*) continue ;; esac
          case "$flags" in
            *E*) ;;
            *)
              offmain=1
              case "$flags" in *R*) has_remote=1 ;; esac
              stat="$(absorbed_of "$cid")"
              case "$stat" in
                0) ;;
                1) has_work=1 ;;
                *)
                  has_work=1
                  classify_ok=0
                  ;;
              esac
              ;;
          esac
        done
        if [ "$st_conflict" = 1 ]; then
          st_state=conflict
        elif [ "$offmain" = 0 ]; then
          st_state=empty
        elif [ "$has_work" = 0 ]; then
          st_state=merged
        elif [ "$has_remote" = 1 ]; then
          st_state=in-flight
        else
          st_state=unpushed
        fi
      }
      # Classify a workspace with one jj call. Prints "<state>\t<desc>". The
      # optional second argument is an already-known description (the board has
      # it from the workspace list), which saves one jj process per workspace.
      # Non-zero means the classification input was unreadable; callers must not
      # treat that as any particular state.
      ws_state() {
        local name="$1" rev="$1@" desc lines have_desc=0
        if [ "$#" -ge 2 ]; then
          have_desc=1
          desc="$2"
        fi
        if [ "$have_desc" = 0 ]; then
          desc="$(jj --ignore-working-copy log --no-graph -r "$rev" -T 'description.first_line()' 2>/dev/null)" || return 1
        fi
        lines="$(jj --ignore-working-copy log --no-graph -r "$rev | (ancestors($rev) ~ ::$trunk)" \
          -T "commit_id ++ \" \" ++ if(self.conflict(), \"C\", \"-\") ++ if(self.empty(), \"E\", \"-\") ++ if(self.remote_bookmarks(), \"R\", \"-\") ++ if(self.contained_in(\"::$trunk\"), \"M\", \"-\") ++ \"\n\"" 2>/dev/null)" || return 1
        [ -n "$lines" ] || return 1
        state_from_flags <<<"$lines"
        printf '%s\t%s\n' "$st_state" "$desc"
      }
      # A workspace "has work" when its state holds content not yet on
      # the trunk. done/rm refuse to reclaim such a workspace; an unreadable
      # classification counts as work, so a transient jj failure cannot turn a
      # removal into data loss.
      ws_has_work() {
        local out
        out="$(ws_state "$1")" || return 0
        case "''${out%%$'\t'*}" in
          unpushed | in-flight | conflict) return 0 ;;
          *) return 1 ;;
        esac
      }
      # A live session-or-agent process whose cwd is inside the workspace means
      # a session still uses it, even when its work has already merged, so gc
      # must not reclaim under it. Auxiliary daemons (per-project MCP and LSP
      # servers) inherit their spawning session's cwd and outlive it, so the
      # cwd-only check excludes them: live_exclude is the default preset for
      # shipped aux binaries, and ROOKERY_LIVE_EXCLUDE adds patterns later without
      # a code change. A process whose cmdline cannot be read counts as live -
      # an unreadability distinction cannot be made here, and rookery never kills.
      #
      # /proc is captured once per invocation. The old per-workspace loop forked
      # readlink for every PID of every workspace, about ten seconds on a 37
      # workspace fleet; one find pass resolves every readable cwd and indexes
      # each one under all of its ancestors, so a workspace lookup is an array
      # read plus a cmdline check for its own processes. The board snapshots
      # concurrently with its jj calls and hands the file to proc_load; gc falls
      # back to proc_scan on first use.
      live_exclude='(^|/)[^ ]*(mcp-[a-z0-9._-]+-wrapped|nix-language-server)[^ ]*'
      [ -n "''${ROOKERY_LIVE_EXCLUDE:-}" ] && live_exclude="$live_exclude|''${ROOKERY_LIVE_EXCLUDE}"
      declare -A proc_pids_of=()
      declare -A board_roots=()
      proc_loaded=0
      proc_load() {
        local path cwd pid dir
        proc_loaded=1
        while IFS=$'\t' read -r path cwd; do
          [ -n "$cwd" ] || continue
          pid="''${path#/proc/}"
          pid="''${pid%%/*}"
          dir="$cwd"
          if [ "''${#board_roots[@]}" -eq 0 ]; then
            # Unrestricted (gc): index every ancestor, so any later workspace
            # lookup is one array read.
            while [ -n "$dir" ] && [ "$dir" != / ]; do
              proc_pids_of["$dir"]+="$pid "
              dir="''${dir%/*}"
            done
            continue
          fi
          # Restricted (the board): the caller filtered the snapshot to the
          # workspace parents, so attribute each line to its deepest root.
          while [ -n "$dir" ] && [ "$dir" != / ]; do
            if [ -n "''${board_roots[$dir]:-}" ]; then
              proc_pids_of["$dir"]+="$pid "
              break
            fi
            dir="''${dir%/*}"
          done
        done
      }
      proc_scan() {
        [ "$proc_loaded" = 1 ] && return 0
        proc_load < <(find /proc/[0-9]*/cwd -maxdepth 0 -printf '%p\t%l\n' 2>/dev/null || true)
      }
      ws_live() {
        local target="$1" pid cmd
        local -a argv=()
        proc_scan
        for pid in ''${proc_pids_of[$target]:-}; do
          argv=()
          mapfile -d "" -t argv 2>/dev/null < "/proc/$pid/cmdline" || argv=()
          cmd="''${argv[*]}"
          [[ $cmd =~ $live_exclude ]] && continue
          return 0
        done
        return 1
      }
      # A recent opencode session rooted in the workspace is a resumable
      # conversation even when no process holds the directory: the TUI may exit
      # while a permission prompt is pending, or the client may run elsewhere and
      # be pointed at this worktree. Pending input is in-memory, so the session's
      # last update is the observable signal; the window is ROOKERY_GC_SESSION_AGE.
      # Guard-silent: a missing database, a missing sqlite3, or a schema change
      # falls back to the process and touch checks. One query covers every
      # directory, instead of one sqlite process per workspace.
      session_db="''${XDG_DATA_HOME:-$HOME/.local/share}/opencode/opencode.db"
      session_age_seconds() {
        v="''${ROOKERY_GC_SESSION_AGE:-604800}"
        if printf '%s' "$v" | grep -Eq '^[0-9]+$'; then
          printf '%s' "$v"
        else
          echo 604800
        fi
      }
      declare -A session_dirs=()
      session_loaded=0
      session_query() {
        local since
        [ -r "$session_db" ] || return 0
        command -v sqlite3 >/dev/null 2>&1 || return 0
        since=$(( ( $(date +%s) - $(session_age_seconds) ) * 1000 ))
        sqlite3 -cmd '.timeout 2000' "file:$session_db?mode=ro" \
          "select distinct directory from session_v2 where time_archived is null and time_updated >= $since" 2>/dev/null || true
      }
      session_load() {
        local dir
        session_loaded=1
        while IFS= read -r dir; do
          [ -n "$dir" ] || continue
          # The board knows its roots, so only those directories earn an
          # associative entry; gc (no root set) keeps them all.
          if [ "''${#board_roots[@]}" -eq 0 ] || [ -n "''${board_roots[$dir]:-}" ]; then
            session_dirs["$dir"]=1
          fi
        done
      }
      ws_session_recent() {
        [ "$session_loaded" = 1 ] || session_load < <(session_query)
        [ -n "''${session_dirs[$1]:-}" ]
      }
      # The JSON board carries the conversation title and last update, not just
      # the hold boolean. Same recent-session window as the hold probe; the
      # grouped query returns one row per directory. `-json` keeps titles with
      # tabs or newlines parseable.
      declare -A session_meta=()
      session_meta_query() {
        local since
        [ -r "$session_db" ] || return 0
        command -v sqlite3 >/dev/null 2>&1 || return 0
        since=$(( ( $(date +%s) - $(session_age_seconds) ) * 1000 ))
        sqlite3 -cmd '.timeout 2000' -json "file:$session_db?mode=ro" \
          "select directory, title, max(time_updated) as updated from session_v2 where time_archived is null and time_updated >= $since group by directory" 2>/dev/null || true
      }
      session_meta_load() {
        local entry dir
        session_loaded=1
        while IFS= read -r entry; do
          [ -n "$entry" ] || continue
          dir="$(printf '%s' "$entry" | jq -r '.directory // empty' 2>/dev/null)" || continue
          [ -n "$dir" ] || continue
          if [ "''${#board_roots[@]}" -eq 0 ] || [ -n "''${board_roots[$dir]:-}" ]; then
            session_dirs["$dir"]=1
            session_meta["$dir"]="$entry"
          fi
        done < <(jq -c '.[]?' "$1" 2>/dev/null || true)
      }
      # True when any entry under the workspace changed at or after the epoch.
      # GNU find's -newermt covers files and directories, so an edit, a new file
      # or a jj snapshot all count as activity, unlike the directory mtime alone.
      ws_touched_since() {
        [ -n "$(find "$1" -newermt "@$2" -print -quit 2>/dev/null)" ]
      }
      # --- board classification and its cache -------------------------------

      # States are the only thing cached: they are a pure function of the jj
      # operation, the trunk and the workspace targets in the key. Anything
      # that changes without a jj operation - processes, recent sessions, wait
      # markers - is recomputed on every run, so a cache hit cannot show a stale
      # hold. The key moves on a commit or description change (the workspace
      # list carries each target commit), a bookmark move or fetch (the op id
      # and the trunk), and a workspace add, forget or rename (the list).
      # A cache hit is exact, not time based. Bump the v1 header whenever the
      # state logic changes: the key cannot see a code change by itself.
      board_tmp=""
      board_lock_fd=""
      board_cache_dir=""
      board_cache_file=""
      board_lock_file=""
      declare -A board_states=()
      declare -A cls_parent_of=()
      declare -A cls_flags_of=()
      declare -A cls_target_of=()
      trap '[ -n "$board_tmp" ] && rm -rf "$board_tmp" || true' EXIT

      board_jobs() {
        local v="''${ROOKERY_BOARD_JOBS:-8}"
        printf '%s' "$v" | grep -Eq '^[0-9]{1,3}$' || v=8
        if [ "$v" -lt 1 ]; then v=8; fi
        if [ "$v" -gt 32 ]; then v=32; fi
        printf '%s' "$v"
      }
      # A missing, corrupt or unreadable cache is simply a miss: the board must
      # never fail because of it.
      board_cache_load() {
        local key="$1" body line name st
        board_states=()
        [ -r "$board_cache_file" ] || return 1
        body="$(<"$board_cache_file")" || return 1
        line="''${body%%$'\n'*}"
        [ "$line" = "v1 $key" ] || return 1
        body="''${body#*$'\n'}"
        while IFS=$'\t' read -r name st; do
          [ -n "$name" ] || continue
          case "$st" in
            empty | merged | unpushed | in-flight | conflict) board_states["$name"]="$st" ;;
            *)
              board_states=()
              return 1
              ;;
          esac
        done <<<"$body"
        for name in "''${ws_names[@]}"; do
          if [ -z "''${board_states[$name]+x}" ]; then
            board_states=()
            return 1
          fi
        done
        return 0
      }
      # Write through a temp file in the same directory and rename, so a reader
      # sees either the old cache or the new one, never a partial file.
      board_cache_store() {
        local key="$1" file name
        mkdir -p "$board_cache_dir" 2>/dev/null || return 0
        file="$(mktemp "$board_cache_file.XXXXXX" 2>/dev/null)" || return 0
        {
          printf 'v1 %s\n' "$key"
          for name in "''${ws_names[@]}"; do
            printf '%s\t%s\n' "$name" "''${board_states[$name]}"
          done
        } >"$file" 2>/dev/null || {
          rm -f "$file"
          return 0
        }
        mv -f "$file" "$board_cache_file" 2>/dev/null || rm -f "$file"
        return 0
      }
      # Hold the scan lock so two popups do not classify at once. A popup must
      # not hang: after two seconds the caller classifies without the lock, and
      # the atomic rename makes a duplicate write harmless. flock releases on
      # process death, which a marker-directory lock would not.
      board_lock() {
        command -v flock >/dev/null 2>&1 || return 1
        mkdir -p "$board_cache_dir" 2>/dev/null || return 1
        # A read-only cache directory cannot hold the lock; classify without it.
        # The group, not a trailing 2>/dev/null, silences the open's own failure.
        [ -w "$board_cache_dir" ] || return 1
        { exec {board_lock_fd}>"$board_lock_file"; } 2>/dev/null || {
          board_lock_fd=""
          return 1
        }
        if flock -w 2 "$board_lock_fd"; then
          return 0
        fi
        exec {board_lock_fd}>&- 2>/dev/null || true
        board_lock_fd=""
        return 1
      }
      board_unlock() {
        if [ -n "$board_lock_fd" ]; then
          exec {board_lock_fd}>&- 2>/dev/null || true
        fi
        board_lock_fd=""
      }
      # One jj log over every workspace target and every off-trunk commit
      # reachable from one. The template carries parent edges plus the C/E/R/M
      # flags and attributes exact targets through working_copies(), so no
      # workspace name enters a revset or a generated template, and the cost
      # scales with commits rather than workspaces times commits. Absorption is
      # the only per-commit jj work left and runs in a bounded pool; its results
      # land in absorbed-<commit> files that absorbed_of reads.
      board_classify() {
        local dir="$1" cid parents flags name_list name
        local -a commit_ids=() tnames=() pids=()
        local jobs
        classify_ok=1
        board_states=()
        cls_parent_of=()
        cls_flags_of=()
        cls_target_of=()
        if ! jj --ignore-working-copy log --no-graph \
          -r "working_copies() | ($trunk..working_copies())" \
          -T "commit_id ++ \"\t\" ++ self.parents().map(|p| p.commit_id()).join(\",\") ++ \"\t\" ++ if(self.conflict(), \"C\", \"-\") ++ if(self.empty(), \"E\", \"-\") ++ if(self.remote_bookmarks(), \"R\", \"-\") ++ if(self.contained_in(\"::$trunk\"), \"M\", \"-\") ++ \"\t\" ++ self.working_copies().map(|w| w.name()).join(\",\") ++ \"\n\"" \
          >"$dir/fleet" 2>/dev/null; then
          echo "rookery: cannot classify workspaces" >&2
          return 1
        fi
        while IFS=$'\t' read -r cid parents flags name_list; do
          [ -n "$cid" ] || continue
          cls_parent_of["$cid"]="$parents"
          cls_flags_of["$cid"]="$flags"
          IFS=',' read -r -a tnames <<<"$name_list"
          for name in "''${tnames[@]}"; do
            [ -n "$name" ] && cls_target_of["$name"]="$cid"
          done
          case "$flags" in
            *M* | *E*) ;;
            *) commit_ids+=("$cid") ;;
          esac
        done <"$dir/fleet"

        jobs="$(board_jobs)"
        for cid in "''${commit_ids[@]}"; do
          (
            if ws_commit_absorbed "$cid"; then
              printf '0'
            else
              printf '%s' "$?"
            fi
          ) >"$dir/absorbed-$cid" 2>/dev/null &
          pids+=($!)
          if [ "''${#pids[@]}" -ge "$jobs" ]; then
            wait "''${pids[0]}" || true
            pids=("''${pids[@]:1}")
          fi
        done
        wait
        absorb_dir="$dir"
        for i in "''${!ws_names[@]}"; do
          name="''${ws_names[$i]}"
          if ! board_state_for "$name" "''${targets[$i]}"; then
            echo "rookery: cannot classify workspace '$name'" >&2
            absorb_dir=""
            return 1
          fi
          board_states["$name"]="$st_state"
        done
        absorb_dir=""
        return 0
      }
      # Rebuild the per-workspace line set ws_state would get from jj: the
      # target plus the off-trunk commits reachable from it. A trunk commit is
      # included only as the target itself and stops the walk, which matches
      # "T | (ancestors(T) ~ ::trunk)". Order never matters here, so the
      # fleet scan needs no worker ordering.
      #
      # The target comes from the workspace list, not only from the fleet
      # template: jj renders an empty working-copy name for the sole primary
      # workspace, so a fresh repository with no secondary has no mapping in
      # cls_target_of and would otherwise be unclassifiable.
      board_state_for() {
        local name="$1" start="''${2:-''${cls_target_of[$1]:-}}" cid parents flags lines=""
        local -a queue=() seen=()
        [ -n "$start" ] || return 1
        queue=("$start")
        while [ "''${#queue[@]}" -gt 0 ]; do
          cid="''${queue[0]}"
          queue=("''${queue[@]:1}")
          if [ -z "''${cls_flags_of[$cid]+x}" ]; then continue; fi
          case " ''${seen[*]} " in
            *" $cid "*) continue ;;
          esac
          seen+=("$cid")
          flags="''${cls_flags_of[$cid]}"
          if [ "$cid" != "$start" ]; then
            case "$flags" in *M*) continue ;; esac
          fi
          lines+="$cid $flags"$'\n'
          case "$flags" in *M*) continue ;; esac
          for parents in ''${cls_parent_of[$cid]}; do
            queue+=("$parents")
          done
        done
        state_from_flags <<<"$lines"
        return 0
      }
      # Read-only overview of every workspace and its state. Three renderings
      # share one classification pass: table (the default listing), counts
      # (integers "total empty merged unpushed in-flight conflict stale waitold"
      # for the status badge, where waitold counts parked workspaces past
      # ROOKERY_WAIT_STALE_DAYS) and board (sections by what needs attention, so a
      # returning user finds the one entry that matters without reading a linear
      # list against the fleet).
      emit_ls() {
        local mode="$1" refresh="$2"
        local name state desc ws_root live marker suffix state_display
        local target
        local wait_parked wait_created wait_reason wait_days
        local n_total=0 n_empty=0 n_merged=0 n_unpushed=0 n_inflight=0 n_conflict=0 n_stale=0 n_waiting=0 n_waitold=0
        local now stale_days i
        local op_id main_cid repo_path repo_id key hit
        local clean_marker clean_created clean_age ws_ts ws_age task_id session_entry json_session json_wait json_clean
        local -a ws_names=() roots=() descs=() targets=() raw_roots=() stamps=()
        local rows=()
        local row
        local rows_parked=() rows_progress=() rows_review=() rows_conflict=() rows_claimed=() rows_reclaim=()
        local tmp
        now="$(date +%s)"
        stale_days="$(wait_stale_days)"
        tmp="$(mktemp -d 2>/dev/null)" || {
          echo "rookery: cannot create a working directory" >&2
          return 1
        }
        board_tmp="$tmp"
        # The workspace list, the cache-key calls and the session probe are
        # independent; starting them together hides the probe and key cost
        # behind the jj calls. The probes are loaded only after the
        # classification and are never cached.
        (
          if [ "$mode" = "json" ]; then
            session_meta_query >"$tmp/sessions"
          else
            session_query >"$tmp/sessions"
          fi
        ) &
        (
          jj --ignore-working-copy workspace list \
            -T 'name ++ "\t" ++ root ++ "\t" ++ target.commit_id() ++ "\t" ++ target.committer().timestamp().format("%s") ++ "\t" ++ target.description().first_line() ++ "\n"' \
            >"$tmp/ws" 2>/dev/null || true
        ) &
        (
          jj --ignore-working-copy op log --no-graph -n 1 -T 'id' >"$tmp/op" 2>/dev/null || true
        ) &
        (
          jj --ignore-working-copy log --no-graph -r "$trunk" -T 'commit_id' >"$tmp/main" 2>/dev/null || true
        ) &
        (
          jj config path --repo >"$tmp/repopath" 2>/dev/null || true
        ) &
        wait
        if [ ! -s "$tmp/ws" ]; then
          echo "rookery: cannot list jj workspaces" >&2
          return 1
        fi
        while IFS=$'\t' read -r name ws_root target ws_ts desc; do
          [ -n "$name" ] || continue
          ws_names+=("$name")
          targets+=("$target")
          descs+=("$desc")
          stamps+=("$ws_ts")
          if [ -n "$ws_root" ] && [ "$ws_root" != none ]; then
            raw_roots+=("$ws_root")
          else
            raw_roots+=("$(ws_dir_for "$name")")
          fi
        done <"$tmp/ws"
        if [ "''${#ws_names[@]}" -eq 0 ]; then
          echo "rookery: no jj workspaces found" >&2
          return 1
        fi
        mapfile -t roots < <(realpath -m -- "''${raw_roots[@]}")
        if [ "''${#roots[@]}" -ne "''${#ws_names[@]}" ]; then
          echo "rookery: cannot resolve workspace roots" >&2
          return 1
        fi
        # The three key components move exactly when a jj-derived state can
        # change. The repo id is the path jj keeps repo-scoped config under,
        # which every workspace of the repository shares (the workspace root is
        # not shared, so hashing it would fragment the cache per workspace).
        main_cid="$(<"$tmp/main")"
        if [ -z "$main_cid" ]; then
          echo "rookery ls: $trunk not found; run 'jj git fetch' first" >&2
          return 1
        fi
        op_id="$(<"$tmp/op")"
        repo_path="$(<"$tmp/repopath")"
        [ -n "$repo_path" ] || repo_path="$root"
        # jj keeps repo-scoped config under a directory named after the
        # repository, shared by every workspace of it; that name is the cache
        # identity (a workspace root is not shared).
        repo_id=""
        case "$repo_path" in
          */config.toml)
            repo_id="''${repo_path%/config.toml}"
            repo_id="''${repo_id##*/}"
            ;;
        esac
        [ -n "$repo_id" ] || repo_id="$(printf '%s' "$root" | sha256sum | cut -c1-16)"
        board_cache_dir="''${XDG_CACHE_HOME:-$HOME/.cache}/rookery"
        board_cache_file="$board_cache_dir/board-$repo_id"
        board_lock_file="$board_cache_dir/board-$repo_id.lock"
        key="$(
          {
            printf 'op %s\nmain %s\n' "$op_id" "$main_cid"
            for i in "''${!ws_names[@]}"; do
              printf 'ws %s\t%s\t%s\n' "''${ws_names[$i]}" "''${roots[$i]}" "''${targets[$i]}"
            done
          } | sha256sum
        )"
        key="''${key%% *}"

        # The process snapshot is filtered to the workspace parents by find
        # itself: a readlink pass over /proc is cheap, but handing the board
        # hundreds of unrelated cwds to attribute is not. It overlaps the
        # classification below.
        local -a find_args=()
        local -A find_parents=()
        local parent
        for ws_root in "''${roots[@]}"; do
          parent="''${ws_root%/*}"
          [ -n "''${find_parents[$parent]:-}" ] && continue
          find_parents["$parent"]=1
          if [ "''${#find_args[@]}" -eq 0 ]; then
            find_args=(-lname "$parent/*")
          else
            find_args+=(-o -lname "$parent/*")
          fi
        done
        (
          if [ "''${#find_args[@]}" -eq 0 ]; then
            find /proc/[0-9]*/cwd -maxdepth 0 -printf '%p\t%l\n'
          else
            find /proc/[0-9]*/cwd -maxdepth 0 \( "''${find_args[@]}" \) -printf '%p\t%l\n'
          fi >"$tmp/proc" 2>/dev/null || true
        ) &

        board_states=()
        hit=0
        if [ "$refresh" = 0 ] && [ -n "$op_id" ] && board_cache_load "$key"; then
          hit=1
        fi
        if [ "$hit" = 0 ]; then
          if [ -n "$op_id" ] && board_lock; then
            if [ "$refresh" = 0 ] && board_cache_load "$key"; then
              hit=1
            fi
            if [ "$hit" = 0 ]; then
              if ! board_classify "$tmp"; then
                board_unlock
                return 1
              fi
              if [ "$classify_ok" = 1 ]; then
                board_cache_store "$key"
              fi
            fi
            board_unlock
          else
            board_classify "$tmp" || return 1
          fi
        fi
        wait
        board_roots=()
        for ws_root in "''${roots[@]}"; do
          board_roots["$ws_root"]=1
        done
        proc_load <"$tmp/proc"
        if [ "$mode" = "json" ]; then
          session_meta_load "$tmp/sessions"
          store_ws_map
        else
          session_load <"$tmp/sessions"
        fi

        for i in "''${!ws_names[@]}"; do
          name="''${ws_names[$i]}"
          state="''${board_states[$name]:-}"
          if [ -z "$state" ]; then
            echo "rookery: no classification for workspace '$name'" >&2
            return 1
          fi
          desc="''${descs[$i]}"
          ws_root="''${roots[$i]}"
          live="-"
          ws_live "$ws_root" && live="live"
          if [ "$live" = "-" ] && { [ "$state" = "empty" ] || [ "$state" = "merged" ]; } && ws_session_recent "$ws_root"; then
            live="session"
          fi

          # Parked: an explicit wait marker overrides the commit state for the
          # display and shields the workspace from gc entirely - the marker
          # outclasses the age gate and the liveness scan below.
          wait_parked=0
          wait_created=0
          wait_reason=""
          printf -v marker '%s/%s.wait' "$base" "$name"
          if [ -f "$marker" ]; then
            wait_parked=1
            wait_reason="$(sed -n '1p' "$marker")"
            wait_created="$(sed -n '2p' "$marker")"
            if ! printf '%s' "$wait_created" | grep -Eq '^[0-9]+$'; then wait_created=0; fi
          fi
          wait_days=$(( (now - wait_created) / 86400 ))

          clean_marker="$(clean_marker_for "$name")"
          clean_age=""
          if [ -f "$clean_marker" ]; then
            clean_created="$(sed -n '1p' "$clean_marker")"
            if printf '%s' "$clean_created" | grep -Eq '^[0-9]+$'; then
              clean_age=$(( now - clean_created ))
            else
              clean_age=$now
            fi
          fi

          if [ "$wait_parked" = 1 ]; then
            state_display="waiting"
            suffix=""
            [ "$wait_days" -gt 0 ] && suffix=" (''${wait_days}d)"
            desc_display="parked: ''${wait_reason:-(no reason given)}$suffix"
            [ "$state" != "empty" ] && desc_display="$desc_display [jj: $state]"
          else
            state_display="$state"
            desc_display="$desc"
            [ -n "$desc_display" ] || desc_display="(no description set)"
          fi

          if [ "$mode" = "json" ]; then
            session_entry="''${session_meta[$ws_root]:-}"
            json_session="null"
            if [ -n "$session_entry" ]; then
              json_session="$(printf '%s' "$session_entry" | jq -c '{title: (.title // null), updated: (.updated // null)}' 2>/dev/null || printf 'null')"
            fi
            json_wait="null"
            if [ "$wait_parked" = 1 ]; then
              json_wait="$(jq -cn --arg reason "$wait_reason" --argjson age "$(( now - wait_created ))" '{reason:$reason,age:$age}')"
            fi
            json_clean="null"
            [ -n "$clean_age" ] && json_clean="$(jq -cn --argjson age "$clean_age" '{age:$age}')"
            task_id="''${task_of_ws[$name]:-}"
            ws_ts="''${stamps[$i]:-0}"
            case "$ws_ts" in
              "" | *[!0-9]*) ws_ts=0 ;;
            esac
            ws_age=$(( now - ws_ts ))
            [ "$ws_age" -lt 0 ] && ws_age=0
            jq -cn \
              --arg task "$task_id" \
              --arg workspace "$name" \
              --arg path "$ws_root" \
              --arg state "$state" \
              --arg hold "$live" \
              --arg description "$desc" \
              --argjson session "$json_session" \
              --argjson wait "$json_wait" \
              --argjson clean "$json_clean" \
              --argjson age "$ws_age" \
              '{task: (if $task == "" then null else $task end), workspace: $workspace, path: $path, state: $state, hold: $hold, description: $description, session: $session, wait: $wait, clean: $clean, age: $age}'
            continue
          fi

          if [ "$name" != "default" ]; then
            n_total=$((n_total + 1))
            case "$state" in
              empty) n_empty=$((n_empty + 1)) ;;
              merged) n_merged=$((n_merged + 1)) ;;
              unpushed) n_unpushed=$((n_unpushed + 1)) ;;
              in-flight) n_inflight=$((n_inflight + 1)) ;;
              conflict) n_conflict=$((n_conflict + 1)) ;;
            esac
            if [ "$wait_parked" = 1 ]; then
              n_waiting=$((n_waiting + 1))
              [ "$wait_days" -ge "$stale_days" ] && n_waitold=$((n_waitold + 1))
            fi
            # Stale: gc would reclaim it now if not for the age gate - i.e. it
            # is empty/merged and nothing is rooted in it. Live sessions, waiting
            # parkers and young workspaces are excluded so the badge does not nag.
            if [ "$live" = "-" ] && [ "$wait_parked" = 0 ] && { [ "$state" = "empty" ] || [ "$state" = "merged" ]; }; then
              n_stale=$((n_stale + 1))
            fi
            if [ "$wait_parked" = 1 ]; then
              printf -v row '  %-16s %s' "$name" "$desc_display"
              rows_parked+=("$row")
            else
              case "$state" in
                unpushed)
                  printf -v row '  %-16s %s' "$name" "$desc_display"
                  rows_progress+=("$row")
                  ;;
                in-flight)
                  printf -v row '  %-16s %s' "$name" "$desc_display"
                  rows_review+=("$row")
                  ;;
                conflict)
                  printf -v row '  %-16s %s' "$name" "$desc_display"
                  rows_conflict+=("$row")
                  ;;
                empty | merged)
                  printf -v row '  %-16s %s' "$name" "$desc_display"
                  if [ "$live" != "-" ]; then
                    rows_claimed+=("$row")
                  else
                    rows_reclaim+=("$row")
                  fi
                  ;;
              esac
            fi
          fi
          printf -v row '  %-18s %-10s %-8s %s' "$name" "$state_display" "$live" "$desc_display"
          rows+=("$row")
        done

        if [ "$mode" = "json" ]; then
          return 0
        fi

        if [ "$mode" = counts ]; then
          printf '%s %s %s %s %s %s %s %s\n' "$n_total" "$n_empty" "$n_merged" "$n_unpushed" "$n_inflight" "$n_conflict" "$n_stale" "$n_waitold"
          return
        fi

        if [ "$mode" = board ]; then
          echo "rookery board - state derived from jj plus the wait marker and recent sessions; cleanup happens only under RECLAIMABLE"
          if [ "''${#rows_parked[@]}" -gt 0 ]; then
            echo ""
            echo "  PARKED (waiting on the outside world)"
            printf '%s\n' "''${rows_parked[@]}"
          fi
          if [ "''${#rows_progress[@]}" -gt 0 ]; then
            echo ""
            echo "  IN PROGRESS (unpushed)"
            printf '%s\n' "''${rows_progress[@]}"
          fi
          if [ "''${#rows_review[@]}" -gt 0 ]; then
            echo ""
            echo "  IN REVIEW (open PR)"
            printf '%s\n' "''${rows_review[@]}"
          fi
          if [ "''${#rows_conflict[@]}" -gt 0 ]; then
            echo ""
            echo "  NEEDS YOU (conflict)"
            printf '%s\n' "''${rows_conflict[@]}"
          fi
          if [ "''${#rows_claimed[@]}" -gt 0 ]; then
            echo ""
            echo "  CLAIMED (session active)"
            printf '%s\n' "''${rows_claimed[@]}"
          fi
          if [ "''${#rows_reclaim[@]}" -gt 0 ]; then
            echo ""
            echo "  RECLAIMABLE (gc removes once idle and unoccupied)"
            printf '%s\n' "''${rows_reclaim[@]}"
          fi
          return
        fi

        echo "Workspace states:"
        printf '  %-18s %-10s %-8s %s\n' NAME STATE HOLD DESCRIPTION
        printf '%s\n' "''${rows[@]}"
        echo ""
        echo "  empty/merged: reclaimable by 'rookery gc' once idle and unoccupied"
        echo "  HOLD:         live = process rooted here, session = recent opencode session, - = idle"
        echo "  unpushed:     commits outside $trunk no remote bookmark tracks"
        echo "  in-flight:    commits outside $trunk with an open PR"
        echo "  conflict:     needs 'jj resolve'"
        echo "  waiting:      parked with 'rookery wait'; gc keeps until 'rookery go'"
        echo ""
        echo "Summary: $n_empty empty, $n_merged merged, $n_unpushed unpushed, $n_inflight in-flight, $n_conflict conflict, $n_stale stale, $n_waiting waiting ($n_waitold reminder)"
      }
      # Optional lifecycle hook, run with `bash -c` and ROOKERY_TASK/DIR/ROOT/BASE
      # exported (the OCWS_* spellings stay for hooks written before the rename).
      # Env-only so there is no config file to parse. A failing hook is warned
      # about, not fatal: a broken setup step must not strand a workspace or
      # block gc.
      run_hook() {
        local hook="$1" task="$2" dir="$3" cmd="$4"
        [ -n "$cmd" ] || return 0
        ROOKERY_TASK="$task" ROOKERY_DIR="$dir" ROOKERY_ROOT="$root" ROOKERY_BASE="$base" \
          OCWS_TASK="$task" OCWS_DIR="$dir" OCWS_ROOT="$root" OCWS_BASE="$base" \
          bash -c "$cmd" || echo "rookery: '$hook' hook failed" >&2
      }
      # A workspace inherits the tracked .envrc, but direnv has never approved
      # the new path: an interactive shell prints a block error on every prompt
      # and never enters the dev shell. Approve only content that still matches
      # the trunk, so an agent-edited .envrc keeps the normal direnv prompt.
      approve_envrc() {
        local dir="$1"
        [ -f "$dir/.envrc" ] || return 0
        (cd "$dir" && jj --ignore-working-copy file show -r "$trunk" .envrc 2>/dev/null) \
          | cmp -s - "$dir/.envrc" || return 0
        "${direnv}/bin/direnv" allow "$dir" >/dev/null 2>&1 || true
      }
      rip_workspace() {
        local task="$1" dir wait_marker clean_marker
        dir="$(realpath -m "$(ws_root_for "$task")")"
        if [ "$dir" = "$(realpath -m "$root")" ]; then
          echo "rookery: refusing to remove the current workspace ('$task')" >&2
          exit 1
        fi
        run_hook pre_delete "$task" "$dir" "''${ROOKERY_HOOK_PRE_DELETE:-}"
        # All removal paths (done, rm, gc, clean --apply) funnel through here, so
        # the wait marker cannot survive the workspace: insistence on clearing
        # it in each caller would eventually forget one, and a stale marker that
        # pulls a workspace away from gc later would be invisible in
        # `rookery ls`. The clean marker follows the same rule.
        wait_marker="$(wait_marker_for "$task")"
        if [ -f "$wait_marker" ]; then rip "$wait_marker"; fi
        clean_marker="$(clean_marker_for "$task")"
        if [ -f "$clean_marker" ]; then rip "$clean_marker"; fi
        # A failed store write must never block reclamation; the row is
        # harmless because the board joins workspaces by the jj list.
        if [ -n "$(store_ws_task "$task")" ]; then
          store_drop_ws "$task" || echo "rookery: could not update the task store for '$task'" >&2
        fi
        jj workspace forget "$task"
        [ -d "$dir" ] && rip "$dir"
        run_hook post_delete "$task" "$dir" "''${ROOKERY_HOOK_POST_DELETE:-}"
      }
      # Forget+rip reclaimable workspaces (state empty or merged) that have sat
      # untouched past the age gate and hold no live process, never the current
      # one. An empty unclaimed workspace is pure overhead and gets the shorter
      # ROOKERY_GC_EMPTY_AGE; a workspace that held work gets the longer
      # ROOKERY_GC_AGE. A workspace whose recorded directory is gone is forgotten
      # outright, wait marker and all. Parked workspaces (a wait marker) are
      # checked before classification and kept whatever the state mentions, so
      # the marker bypasses every gate below it in this loop. Process liveness, a
      # recent opencode session, and the touch-time gate keep a paused
      # conversation from losing its directory; rip's graveyard makes a mistaken
      # removal restorable regardless.
      # do_fetch=1 fetches first so absorption is judged against current origin.
      rookery_gc() {
        local do_fetch="$1" name dir state desc out age_limit now
        local marker wait_reason wait_created wait_days
        now="$(date +%s)"
        # Without the trunk every workspace would classify as empty and be
        # reclaimed. Refuse rather than guess.
        jj --ignore-working-copy log -r "$trunk" -n 0 >/dev/null 2>&1 || {
          echo "rookery gc: $trunk not found; skipping" >&2
          return 0
        }
        [ "$do_fetch" = 1 ] && { jj git fetch >/dev/null 2>&1 || true; }
        while IFS= read -r name; do
          [ "$name" = "default" ] && continue
          dir="$(realpath -m "$(ws_root_for "$name")")"
          [ "$dir" = "$(realpath -m "$root")" ] && continue

          if [ ! -d "$dir" ]; then
            rip_workspace "$name"
            echo "rookery gc: forgot '$name' (directory missing)"
            continue
          fi

          # Parked: a wait marker keeps the workspace out of every gate below,
          # whatever its commit state, because the reason lives outside jj - a
          # session waiting on the outside world looks empty and unoccupied.
          marker="$(wait_marker_for "$name")"
          if [ -f "$marker" ]; then
            wait_reason="$(sed -n '1p' "$marker")"
            wait_created="$(sed -n '2p' "$marker")"
            if ! printf '%s' "$wait_created" | grep -Eq '^[0-9]+$'; then wait_created=0; fi
            echo "rookery gc: keeping '$name' (parked: ''${wait_reason:-(no reason given)})"
            wait_days=$(( (now - wait_created) / 86400 ))
            if [ "$wait_days" -ge "$(wait_stale_days)" ]; then
              echo "rookery gc: '$name' has waited $wait_days days - resume it ('rookery go $name') or release the workspace"
            fi
            continue
          fi

          # A classification that cannot be read keeps the workspace: this loop
          # removes directories, so "unknown" must never mean "empty".
          if ! out="$(ws_state "$name")"; then
            echo "rookery gc: cannot classify '$name'; keeping it" >&2
            continue
          fi
          IFS=$'\t' read -r state desc <<<"$out"
          case "$state" in
            unpushed) echo "rookery gc: keeping '$name' (unpushed work)"; continue ;;
            in-flight) echo "rookery gc: keeping '$name' (open PR)"; continue ;;
            conflict) echo "rookery gc: keeping '$name' (conflict)" >&2; continue ;;
          esac

          if ws_live "$dir"; then
            echo "rookery gc: keeping '$name' (live process)"
            continue
          fi

          if ws_session_recent "$dir"; then
            echo "rookery gc: keeping '$name' (recent session)"
            continue
          fi

          if [ "$state" = "empty" ] && [ -z "$desc" ]; then
            age_limit="''${ROOKERY_GC_EMPTY_AGE:-21600}"
          else
            age_limit="''${ROOKERY_GC_AGE:-86400}"
          fi
          ws_touched_since "$dir" "$((now - age_limit))" && continue

          rip_workspace "$name"
          echo "rookery gc: forgot '$name' ($state)"
        done < <(jj --ignore-working-copy workspace list -T 'name ++ "\n"' 2>/dev/null)
      }
      # The safe sweep behind `clean --apply`: only a workspace the person marked
      # and that still classifies as empty or merged, with no live process and no
      # unpushed, in-flight or conflicted work, is removed. The mark is the
      # consent for the pause gc would otherwise protect through the session-age
      # hold; unreadable state keeps the workspace.
      rookery_clean_apply() {
        local marker name dir out state desc removed=0 kept=0
        for marker in "$base"/*.clean; do
          [ -f "$marker" ] || continue
          name="''${marker##*/}"
          name="''${name%.clean}"
          valid_task "$name" || continue
          if ! workspace_exists "$name"; then
            rip "$marker"
            echo "rookery clean: dropped the stale mark for '$name'"
            continue
          fi
          dir="$(realpath -m "$(ws_root_for "$name")")"
          if [ "$dir" = "$(realpath -m "$root")" ]; then
            echo "rookery clean: keeping '$name' (current workspace)"
            kept=$((kept + 1))
            continue
          fi
          # A parked workspace is waiting on the outside world; the wait marker
          # outranks a cleanup mark, exactly as it outranks the gc gates.
          if [ -f "$(wait_marker_for "$name")" ]; then
            echo "rookery clean: keeping '$name' (parked)"
            kept=$((kept + 1))
            continue
          fi
          if ! out="$(ws_state "$name")"; then
            echo "rookery clean: cannot classify '$name'; keeping it" >&2
            kept=$((kept + 1))
            continue
          fi
          IFS=$'\t' read -r state desc <<<"$out"
          case "$state" in
            unpushed | in-flight | conflict)
              echo "rookery clean: keeping '$name' ($state)"
              kept=$((kept + 1))
              continue
              ;;
          esac
          if ws_live "$dir"; then
            echo "rookery clean: keeping '$name' (live process)"
            kept=$((kept + 1))
            continue
          fi
          rip_workspace "$name"
          echo "rookery clean: removed '$name' ($state)"
          removed=$((removed + 1))
        done
        echo "rookery clean: $removed removed, $kept kept"
        return 0
      }

      sub="''${1:-}"
      [ -n "$sub" ] || usage

      # Optional leading flag: run the opencode 2 beta instead of V1.
      if [ "$sub" = "--v2" ]; then
        ROOKERY_V2=1
        shift
        sub="''${1:-}"
        [ -n "$sub" ] || usage
      fi

      case "$sub" in
        task)
          shift
          action="''${1:-}"
          shift || true
          case "$action" in
            add)
              notes=""
              id=""
              explicit_id=0
              while [ "$#" -gt 0 ]; do
                case "$1" in
                  --id)
                    [ "$#" -ge 2 ] || usage
                    id="$2"
                    explicit_id=1
                    shift 2
                    ;;
                  --notes)
                    [ "$#" -ge 2 ] || usage
                    notes="$2"
                    shift 2
                    ;;
                  -*) usage ;;
                  *) break ;;
                esac
              done
              title="$*"
              [ -n "$title" ] || usage
              [ -n "$id" ] || id="$(task_slug "$title")"
              [ -n "$id" ] || { echo "rookery task add: the title needs a letter or digit" >&2; exit 2; }
              valid_task "$id" || { echo "rookery task add: invalid id '$id' (use [A-Za-z0-9._-])" >&2; exit 2; }
              if [ "$explicit_id" = 1 ] && store_task_exists "$id"; then
                echo "rookery task add: task '$id' already exists" >&2
                exit 2
              fi
              attempt="$id"
              n=2
              while :; do
                if store_task_exists "$attempt"; then
                  attempt="$id-$n"
                  n=$((n + 1))
                  [ "$n" -gt 9 ] && { echo "rookery task add: no free id for '$id'" >&2; exit 1; }
                  continue
                fi
                status=0
                store_add "$attempt" "$title" "$notes" || status=$?
                if [ "$status" = 0 ]; then
                  echo "rookery: task '$attempt' added ($title)"
                  exit 0
                fi
                # 2 is the filter refusing a duplicate that raced the check;
                # anything else is an I/O or parse failure and belongs to the
                # caller.
                [ "$status" = 2 ] || exit "$status"
                attempt="$id-$n"
                n=$((n + 1))
                [ "$n" -gt 9 ] && { echo "rookery task add: no free id for '$id'" >&2; exit 1; }
              done
              ;;
            assign)
              move=0
              if [ "''${1:-}" = "--move" ]; then move=1; shift; fi
              [ "$#" -eq 2 ] || usage
              id="$1"
              ws="$2"
              valid_task "$id" || { echo "rookery task assign: invalid task '$id'" >&2; exit 2; }
              valid_task "$ws" || { echo "rookery task assign: invalid workspace '$ws'" >&2; exit 2; }
              [ "$ws" = "default" ] && { echo "rookery task assign: the primary checkout is not a workspace to group" >&2; exit 1; }
              workspace_exists "$ws" || { echo "rookery task assign: no workspace named '$ws'" >&2; exit 1; }
              status=0
              store_assign "$id" "$ws" "$move" || status=$?
              [ "$status" = 0 ] || exit "$status"
              echo "rookery: '$ws' grouped under '$id'"
              exit 0
              ;;
            list)
              list_json=0
              if [ "''${1:-}" = "--json" ]; then
                list_json=1
                shift
              fi
              [ "$#" -eq 0 ] || usage
              store_check || exit 1
              if [ "$list_json" = 1 ]; then
                store_query '.tasks[]' | jq -c '.'
              else
                store_query '.tasks[] | [.id, .status, ((.workspaces | length) | tostring), .title] | @tsv' \
                  | while IFS=$'\t' read -r id status count title; do
                      [ -n "$id" ] || continue
                      printf '  %-24s %-8s %s %s\n' "$id" "$status" "$count" "$title"
                    done
              fi
              exit 0
              ;;
            done)
              [ "$#" -eq 1 ] || usage
              id="$1"
              store_check || exit 1
              store_task_exists "$id" || {
                echo "rookery task done: no task named '$id'" >&2
                exit 1
              }
              mapfile -t wss < <(store_query '.tasks[] | select(.id == $id) | .workspaces[]' --arg id "$id")
              [ "''${#wss[@]}" -gt 0 ] || {
                echo "rookery task done: '$id' has no workspaces; use 'rookery task archive $id'" >&2
                exit 1
              }
              for ws in "''${wss[@]}"; do
                if ! workspace_exists "$ws"; then
                  echo "rookery: skipping '$ws' (no such workspace)" >&2
                  continue
                fi
                printf '%s\n' "$(date +%s)" > "$(clean_marker_for "$ws")"
                echo "rookery: '$ws' marked for cleanup"
              done
              exit 0
              ;;
            archive)
              [ "$#" -eq 1 ] || usage
              status=0
              store_archive "$1" || status=$?
              [ "$status" = 0 ] || exit "$status"
              echo "rookery: task '$1' archived"
              exit 0
              ;;
            *) usage ;;
          esac
          ;;
        clean)
          shift
          apply=0
          unmark=0
          current=0
          target=""
          while [ "$#" -gt 0 ]; do
            case "$1" in
              --apply) apply=1; shift ;;
              --unmark) unmark=1; shift ;;
              --current) current=1; shift ;;
              -*) usage ;;
              *)
                [ -z "$target" ] || usage
                target="$1"
                shift
                ;;
            esac
          done
          if [ "$apply" = 1 ]; then
            [ -z "$target" ] && [ "$current" = 0 ] && [ "$unmark" = 0 ] || usage
            rookery_clean_apply
            exit 0
          fi
          if [ "$current" = 1 ]; then
            [ -z "$target" ] || usage
            target="$(workspace_at_cwd)" || {
              echo "rookery clean: not inside a registered workspace" >&2
              exit 1
            }
          fi
          [ -n "$target" ] || usage
          valid_task "$target" || { echo "rookery clean: invalid workspace '$target'" >&2; exit 2; }
          [ "$target" = "default" ] && { echo "rookery clean: the primary checkout is the user's" >&2; exit 1; }
          workspace_exists "$target" || { echo "rookery clean: no workspace named '$target'" >&2; exit 1; }
          marker="$(clean_marker_for "$target")"
          if [ "$unmark" = 1 ]; then
            [ -f "$marker" ] || { echo "rookery clean: '$target' is not marked" >&2; exit 1; }
            rip "$marker"
            echo "rookery: '$target' unmarked"
          else
            printf '%s\n' "$(date +%s)" >"$marker"
            echo "rookery: '$target' marked for cleanup"
          fi
          exit 0
          ;;
        open)
          shift
          [ "$#" -eq 1 ] || usage
          target="$1"
          [ "$target" = "default" ] && { echo "rookery open: the primary checkout is not a seat" >&2; exit 1; }
          ws=""
          if workspace_exists "$target"; then
            ws="$target"
          else
            ws="$(store_pick_workspace "$target" 2>/dev/null)" || ws=""
            [ -n "$ws" ] || { echo "rookery open: no workspace or task named '$target'" >&2; exit 1; }
          fi
          herdr_bin="''${HERDR_BIN_PATH:-herdr}"
          command -v "$herdr_bin" >/dev/null 2>&1 || {
            echo "rookery open: herdr binary not found: $herdr_bin" >&2
            exit 1
          }
          dir="$(realpath -m "$(ws_root_for "$ws")")"
          panes="$("$herdr_bin" pane list 2>&1)" || {
            echo "rookery open: cannot read herdr panes: $panes" >&2
            exit 1
          }
          match="$(printf '%s' "$panes" | jq -c --arg dir "$dir" '
            [.result.panes[]?
             | select(((.foreground_cwd // .cwd) // "") as $c
                 | ($c == $dir) or ($c | startswith($dir + "/")))]
            | if length == 0 then empty else .[0] end
          ' 2>/dev/null || true)"
          [ -n "$match" ] || { echo "rookery open: no herdr pane runs in '$ws'" >&2; exit 1; }
          tab="$(printf '%s' "$match" | jq -r '.tab_id // empty')"
          [ -n "$tab" ] || { echo "rookery open: the pane has no tab id" >&2; exit 1; }
          "$herdr_bin" tab focus "$tab" >/dev/null || {
            echo "rookery open: cannot focus tab $tab" >&2
            exit 1
          }
          echo "rookery: opened '$ws'"
          exit 0
          ;;
        wait)
          shift
          task="''${1:-}"
          [ -n "$task" ] || usage
          valid_task "$task" || { echo "rookery: invalid task '$task' (use [A-Za-z0-9._-])" >&2; exit 2; }
          workspace_exists "$task" || { echo "rookery: no workspace named '$task'" >&2; exit 1; }
          [ "$task" = "default" ] && { echo "rookery wait: the primary checkout is the user's, not an agent workspace to park" >&2; exit 1; }
          shift || true
          reason="$*"
          printf '%s\n%s\n' "$reason" "$(date +%s)" > "$(wait_marker_for "$task")"
          echo "rookery: '$task' parked (''${reason:-(no reason given)}) - gc keeps it; resume with 'rookery go $task'"
          exit 0
          ;;
        go)
          shift
          task="''${1:-}"
          [ -n "$task" ] || usage
          valid_task "$task" || { echo "rookery: invalid task '$task' (use [A-Za-z0-9._-])" >&2; exit 2; }
          workspace_exists "$task" || { echo "rookery: no workspace named '$task'" >&2; exit 1; }
          [ "$task" = "default" ] && { echo "rookery go: the primary checkout is the user's, not an agent workspace" >&2; exit 1; }
          wait_marker="$(wait_marker_for "$task")"
          if [ ! -f "$wait_marker" ]; then
            echo "rookery: '$task' is not parked" >&2
            exit 1
          fi
          rip "$wait_marker"
          echo "rookery: '$task' resumed; gc reclassifies it on the next pass"
          exit 0
          ;;
        seat)
          shift
          json=0
          if [ "''${1:-}" = "--json" ]; then
            json=1
            shift
          fi
          [ "$#" -eq 0 ] || usage
          # The shell tool runs in the shared opencode server, not in the pane's
          # client, so its HERDR_* environment can be absent or belong to another
          # pane. The opencode session id is the one identity every tool call
          # carries, and herdr's agent list reports it back with the pane, tab and
          # workspace, so it is the lookup key. A workspace-root match would be
          # wrong here: the live herd reports the same repository cwd for every
          # seat, because `rookery herdr` changes directory inside the pane's shell.
          sid="''${OPENCODE_SESSION_ID:-}"
          if [ -z "$sid" ]; then
            echo "rookery seat: OPENCODE_SESSION_ID is unset; run this from an opencode tool call" >&2
            exit 1
          fi
          herdr_bin="''${HERDR_BIN_PATH:-herdr}"
          command -v "$herdr_bin" >/dev/null 2>&1 || {
            echo "rookery seat: herdr binary not found: $herdr_bin" >&2
            exit 1
          }
          agents="$("$herdr_bin" agent list 2>&1)" || {
            echo "rookery seat: cannot read herdr agents: $agents" >&2
            exit 1
          }
          match="$(printf '%s' "$agents" | jq -c --arg sid "$sid" '
            [.result.agents[]? | select(.agent_session.value == $sid)]
            | if length == 1 then .[0] else empty end
          ' 2>/dev/null || true)"
          if [ -z "$match" ]; then
            count="$(printf '%s' "$agents" | jq -r --arg sid "$sid" \
              '[.result.agents[]? | select(.agent_session.value == $sid)] | length' 2>/dev/null || true)"
            if ! printf '%s' "$count" | grep -Eq '^[0-9]+$'; then
              count=0
            fi
            if [ "$count" -gt 1 ]; then
              echo "rookery seat: session $sid is registered on more than one herdr pane; refusing to guess" >&2
              printf '%s' "$agents" | jq -r --arg sid "$sid" \
                '.result.agents[] | select(.agent_session.value == $sid) | "  \(.pane_id) \(.tab_id) \(.workspace_id) \(.agent_status)"' >&2 || true
            else
              echo "rookery seat: no herdr agent reports opencode session $sid" >&2
              echo "rookery seat: check that tui.herdr.opencodeIntegration is on and the tab is a herdr pane" >&2
            fi
            exit 1
          fi
          here="$(realpath -m "$PWD")"
          names="$(jj --ignore-working-copy workspace list -T 'name ++ "\t" ++ root ++ "\n"' 2>/dev/null)"
          task=""
          while IFS=$'\t' read -r name ws_root; do
            case "$here/" in
              "$(realpath -m "$ws_root")/"*)
                task="$name"
                dir="$(realpath -m "$ws_root")"
                break
                ;;
            esac
          done <<<"$names"
          [ -n "$task" ] || { echo "rookery seat: not inside a registered jj workspace" >&2; exit 1; }
          [ "$task" = "default" ] && { echo "rookery seat: the primary checkout is not a seat" >&2; exit 1; }
          ws="$(printf '%s' "$match" | jq -r '.workspace_id // empty')"
          tab="$(printf '%s' "$match" | jq -r '.tab_id // empty')"
          pane="$(printf '%s' "$match" | jq -r '.pane_id // empty')"
          state="$(printf '%s' "$match" | jq -r '.agent_status // empty')"
          if [ "$json" = 1 ]; then
            jq -nc \
              --arg task "$task" \
              --arg dir "$dir" \
              --arg ws "$ws" \
              --arg tab "$tab" \
              --arg pane "$pane" \
              --arg sid "$sid" \
              --arg state "$state" \
              '{task:$task,dir:$dir,herdr_workspace:$ws,herdr_tab:$tab,herdr_pane:$pane,agent_session:$sid,agent_status:$state,source:"agent-session"}'
          else
            printf '%s\t%s\t%s\t%s\n' "$task" "$tab" "$pane" "$state"
          fi
          exit 0
          ;;
        title)
          shift
          text="$*"
          [ -n "$text" ] || usage
          # The seat runs in its own workspace, so the directory identifies the
          # task: jj records workspace roots absolutely, and a prefix match also
          # covers a command run from a subdirectory. Read the list once: a
          # pipeline here would let an early `break` SIGPIPE jj, and pipefail
          # would then abort the claim.
          here="$(realpath -m "$PWD")"
          names="$(jj --ignore-working-copy workspace list -T 'name ++ "\t" ++ root ++ "\n"' 2>/dev/null)"
          task=""
          while IFS=$'\t' read -r name ws_root; do
            case "$here/" in
              "$(realpath -m "$ws_root")/"*)
                task="$name"
                break
                ;;
            esac
          done <<<"$names"
          [ -n "$task" ] || { echo "rookery title: not inside a registered workspace" >&2; exit 1; }
          # `default` is the coordinator and the anchor the boundaries alias keys
          # off (`default@`), so a stray claim from the primary checkout must not
          # rename it.
          [ "$task" = "default" ] && { echo "rookery title: the primary checkout is not a seat" >&2; exit 1; }
          # Only the placeholder `jj workspace add` wrote is replaceable. Match
          # its shape, not the name, so a retry finishes a claim whose rename
          # already landed. Once a human or an agent described the change, the
          # claim is theirs.
          desc="$(jj --ignore-working-copy log --no-graph -r @ -T 'description.first_line()' 2>/dev/null)"
          # The placeholder task name carries the creation time; a real claim
          # never matches this shape, whatever the trunk is called.
          placeholder="''${desc%%: *}"
          case "$placeholder" in
            t-[0-9]*) ;;
            *) exit 0 ;;
          esac
          printf '%s' "''${placeholder#t-}" | grep -Eq '^[0-9]{4,6}(-[0-9])?$' || exit 0
          case "$desc" in
            *": started from $trunk") ;;
            *) exit 0 ;;
          esac
          slug="$(printf '%s' "$text" \
            | tr '[:upper:]' '[:lower:]' \
            | tr -cs '[:alnum:]' '-' \
            | sed -e 's/^-*//' -e 's/-*$//' \
            | cut -c1-48 \
            | sed -e 's/-*$//')"
          [ -n "$slug" ] || exit 0
          # Workspace names are the handle other sessions read, so a second seat
          # with the same title must not collide with the first.
          taken() {
            local existing
            while IFS=$'\t' read -r existing _; do
              if [ "$existing" = "$1" ]; then return 0; fi
            done <<<"$names"
            return 1
          }
          final="$slug"
          n=2
          while taken "$final"; do
            final="$slug-$n"
            n=$((n + 1))
            [ "$n" -gt 9 ] && break
          done
          # Rename before describing: a refused rename (a name race) then leaves
          # the placeholder intact and the next call retries the whole claim.
          jj workspace rename "$final"
          jj describe -m "$text"
          # A renamed workspace keeps its task grouping; the store keys on the
          # workspace name, so the claim would otherwise orphan the row.
          if [ -n "$(store_ws_task "$task")" ]; then
            store_rename_ws "$task" "$final" || echo "rookery: could not update the task store after the rename" >&2
          fi
          # A manual `rookery title` can run from another tab whose shell happens to
          # be cd'ed into the workspace; relabelling that tab would be wrong. The
          # seat's own tab is the one whose foreground process runs in the
          # workspace, so check that before renaming.
          if [ -n "''${HERDR_TAB_ID:-}" ] && command -v herdr >/dev/null 2>&1; then
            tab_cwd="$(herdr pane list 2>/dev/null | jq -r --arg tab "$HERDR_TAB_ID" \
              'first(.result.panes[]? | select(.tab_id == $tab) | (.foreground_cwd // .cwd)) // empty' 2>/dev/null)"
            case "$(realpath -m "''${tab_cwd:-/nonexistent}")/" in
              "$here/"*) herdr tab rename "$HERDR_TAB_ID" "$final" >/dev/null 2>&1 || true ;;
            esac
          fi
          echo "rookery: claimed '$task' as '$final'"
          exit 0
          ;;
        ls|list)
          shift
          counts=0
          board=0
          json=0
          refresh=0
          while [ "$#" -gt 0 ]; do
            case "$1" in
              --counts)
                counts=1
                shift
                ;;
              --board)
                board=1
                shift
                ;;
              --json)
                json=1
                shift
                ;;
              --refresh)
                refresh=1
                shift
                ;;
              *) usage ;;
            esac
          done
          if [ "$counts" = 0 ] && [ "$board" = 0 ] && [ "$json" = 0 ]; then
            jj --ignore-working-copy workspace list
            echo ""
          fi
          if [ "$counts" = 1 ]; then
            emit_ls counts "$refresh"
          elif [ "$board" = 1 ]; then
            emit_ls board "$refresh"
          elif [ "$json" = 1 ]; then
            emit_ls json "$refresh"
          else
            emit_ls table "$refresh"
          fi
          exit 0
          ;;
        gc)
          shift
          rookery_gc 1
          exit 0
          ;;
        done)
          shift
          force=0
          if [ "''${1:-}" = "--force" ]; then force=1; shift; fi
          task="''${1:-}"
          [ -n "$task" ] || usage
          valid_task "$task" || { echo "rookery: invalid task '$task'" >&2; exit 2; }
          workspace_exists "$task" || { echo "rookery: no workspace named '$task'" >&2; exit 1; }
          jj git fetch >/dev/null 2>&1 || true
          if ws_has_work "$task"; then
            echo "rookery: '$task' still has unmerged/uncommitted work; refusing to remove." >&2
            jj --ignore-working-copy log --no-graph -r "ancestors($task@) ~ ::$trunk ~ empty()" -T '"  " ++ commit_id.short() ++ " " ++ description.first_line() ++ "\n"' >&2
            echo "Merge the PR, then retry; or 'rookery rm --force $task' to discard." >&2
            exit 1
          fi
          # A session can still hold the directory after its work merged; a
          # removal under an open session strands it, so require --force then.
          if [ "$force" != "1" ] && ws_live "$(realpath -m "$(ws_root_for "$task")")"; then
            echo "rookery: a process is still active in '$task'; close it first, or 'rookery done --force $task'." >&2
            exit 1
          fi
          rip_workspace "$task"
          echo "Done: forgot '$task' (directory sent to the graveyard)."
          exit 0
          ;;
        rm|forget)
          shift
          force=0
          if [ "''${1:-}" = "--force" ]; then force=1; shift; fi
          task="''${1:-}"
          [ -n "$task" ] || usage
          valid_task "$task" || { echo "rookery: invalid task '$task'" >&2; exit 2; }
          workspace_exists "$task" || { echo "rookery: no workspace named '$task'" >&2; exit 1; }
          if [ "$force" != "1" ]; then
            if ws_has_work "$task"; then
              echo "rookery: '$task' has unmerged/uncommitted work; use 'rookery done $task' after merging, or 'rookery rm --force $task' to discard." >&2
              exit 1
            fi
            if ws_live "$(realpath -m "$(ws_root_for "$task")")"; then
              echo "rookery: a process is still active in '$task'; close it first, or 'rookery rm --force $task'." >&2
              exit 1
            fi
          fi
          rip_workspace "$task"
          echo "Forgot '$task' (directory sent to the graveyard)."
          exit 0
          ;;
        herdr)
          shift
          # A herdr tab is only a layout: with terminal.new_cwd = follow it
          # inherits the focused pane's worktree, so running opencode in a bare
          # tab shares that working copy. This subcommand creates the tab and
          # starts the agent there through the same creation path as `rookery`, so
          # a seat always gets its own jj workspace. An existing workspace is
          # reopened only when no process is live in it; a second agent in one
          # working copy is the interference this workflow exists to prevent.
          #
          # The seat runs from a herdr popup, and a popup closes the moment its
          # command exits: an empty or invalid answer must re-ask instead of
          # exiting, and a failure after the prompt must hold the popup open
          # for a keypress, or the user sees only a flash of text.
          seat_log="''${XDG_CACHE_HOME:-$HOME/.cache}/rookery/seat.log"
          fail() {
            printf 'rookery herdr: %s\n' "$*" >&2
            if [ -t 0 ]; then
              read -r -p 'press enter to close ' _ || true
            else
              # The detached binding has no terminal to show the error in, so
              # keep it where the next diagnosis will look.
              mkdir -p "$(dirname "$seat_log")"
              printf '%s %s\n' "$(date -Is)" "$*" >> "$seat_log"
            fi
            exit 1
          }
          json=0
          focus=1
          workspace=""
          seat_task=""
          saw_ddash=0
          wait_secs=0
          while [ "$#" -gt 0 ]; do
            case "$1" in
              --json) json=1; shift ;;
              --no-focus) focus=0; shift ;;
              --task)
                [ "$#" -ge 2 ] || usage
                seat_task="$2"
                shift 2
                ;;
              --workspace)
                [ "$#" -ge 2 ] || usage
                workspace="$2"
                shift 2
                ;;
              --wait) wait_secs=180; shift ;;
              --wait=*) wait_secs="''${1#--wait=}"; shift ;;
              --) shift; saw_ddash=1; break ;;
              -*) usage ;;
              *) break ;;
            esac
          done
          case "$wait_secs" in
            *[!0-9]*) usage ;;
          esac
          [ -n "$wait_secs" ] || usage
          if [ -n "$seat_task" ]; then
            valid_task "$seat_task" || { echo "rookery herdr: invalid task '$seat_task' (use [A-Za-z0-9._-])" >&2; exit 2; }
            store_task_open "$seat_task" || {
              echo "rookery herdr: no open task named '$seat_task'" >&2
              exit 1
            }
            # --task always starts a new seat: the task may already own live
            # workspaces, so the workspace name is suffixed until it is free.
            task="$seat_task"
            n=2
            while workspace_exists "$task"; do
              task="$seat_task-$n"
              n=$((n + 1))
              [ "$n" -gt 9 ] && fail "no free workspace name for task '$seat_task'"
            done
            if [ "$#" -gt 0 ] && [ "$saw_ddash" = 0 ]; then
              usage
            fi
            extra=("$@")
          else
            task="''${1:-}"
            if [ -n "$task" ]; then
              shift
            else
              # The keypress is the interrupt: a name prompt would put a dialog in
              # front of every seat. An unnamed seat gets a filler that
              # `rookery title` replaces once OpenCode names the session.
              task="t-$(date +%H%M%S)"
              n=2
              while workspace_exists "$task"; do
                task="t-$(date +%H%M%S)-$n"
                n=$((n + 1))
                [ "$n" -gt 9 ] && break
              done
            fi
            extra=()
            if [ "''${1:-}" = "--" ]; then
              shift
              extra=("$@")
            elif [ "$#" -gt 0 ]; then
              usage
            fi
          fi
          valid_task "$task" || { echo "rookery herdr: invalid task '$task' (use [A-Za-z0-9._-])" >&2; exit 2; }
          # herdr exports HERDR_BIN_PATH to custom command keybindings, which is
          # where the seat popup runs; outside one, fall back to the PATH name.
          herdr_bin="''${HERDR_BIN_PATH:-herdr}"
          command -v "$herdr_bin" >/dev/null 2>&1 || fail "herdr binary not found: $herdr_bin"

          # Start the tab in the primary workspace, not the caller's workspace:
          # the primary's direnv profile is cached, so the seat does not pay a
          # first-load nix develop before the agent starts. The agent still runs
          # only in the workspace rookery creates (or the existing one it cd's to).
          tab_root="$root"
          if workspace_exists default; then
            tab_root="$(realpath -m "$(ws_root_for default)")"
          fi
          [ -d "$tab_root" ] || tab_root="$root"
          approve_envrc "$tab_root"

          if workspace_exists "$task"; then
            dir="$(realpath -m "$(ws_root_for "$task")")"
            approve_envrc "$dir"
            if ws_live "$dir"; then
              fail "'$task' already has a live session in $dir; refusing a second agent in the same workspace"
            fi
            if [ "''${ROOKERY_V2:-0}" = "1" ]; then
              inner="cd $(printf '%q' "$dir") && exec $(printf '%q' opencode2)"
            else
              inner="cd $(printf '%q' "$dir") && exec $(printf '%q' opencode)"
            fi
            for arg in "''${extra[@]}"; do
              inner+=" $(printf '%q' "$arg")"
            done
          else
            dir="$(ws_dir_for "$task")"
            self="$(readlink -f "$0" 2>/dev/null || printf '%s' "$0")"
            # The seat is created for speed: skip the gc pass and the network
            # fetch the create path would otherwise run before `workspace add`.
            inner="ROOKERY_NO_MAINT=1"
            if [ -n "$seat_task" ]; then
              # The workspace is created inside the pane; the create path reads
              # this variable and assigns the new workspace to its task.
              inner+=" ROOKERY_TASK_ASSIGN=$(printf '%q' "$seat_task")"
            fi
            inner+=" $(printf '%q' "$self")"
            [ "''${ROOKERY_V2:-0}" = "1" ] && inner+=" --v2"
            inner+=" $(printf '%q' "$task")"
            if [ "''${#extra[@]}" -gt 0 ]; then
              inner+=" --"
              for arg in "''${extra[@]}"; do
                inner+=" $(printf '%q' "$arg")"
              done
            fi
          fi

          # The tab starts at the primary workspace root: rookery resolves the
          # repository with `jj workspace root`, and the workspace base is
          # outside it. The agent still starts only in the workspace rookery
          # creates (or the existing one it cd's to); nothing runs in the tab's
          # initial directory.
          focus_flag=--focus
          [ "$focus" = 1 ] || focus_flag=--no-focus
          tab_args=(tab create --cwd "$tab_root" --label "$task" "$focus_flag")
          if [ -n "$workspace" ]; then
            tab_args+=(--workspace "$workspace")
          fi
          if ! resp="$("$herdr_bin" "''${tab_args[@]}" 2>&1)"; then
            fail "cannot create a herdr tab: $resp"
          fi
          pane="$(printf '%s' "$resp" | jq -r '.result.root_pane.pane_id // empty' 2>/dev/null || true)"
          tab="$(printf '%s' "$resp" | jq -r '.result.tab.tab_id // empty' 2>/dev/null || true)"
          ws="$(printf '%s' "$resp" | jq -r '.result.tab.workspace_id // empty' 2>/dev/null || true)"
          [ -n "$pane" ] || fail "herdr did not return a root pane: $resp"
          [ -n "$tab" ] || fail "herdr did not return a tab: $resp"
          [ -n "$ws" ] || ws="$workspace"
          if ! "$herdr_bin" pane run "$pane" "$inner"; then
            fail "tab created, but the agent did not start; run 'rookery $task' in it"
          fi
          # herdr has no wait for an agent to appear: agent wait and agent prompt
          # take a target that must already resolve. The pane is the target and
          # agent list is the observable state, so poll it with a deadline rather
          # than handing a lead a pane that cannot be prompted yet. A blocked
          # startup belongs to the person; the tab is kept for inspection.
          state=""
          agent_sid=""
          if [ "$wait_secs" -gt 0 ]; then
            deadline=$(( $(date +%s) + wait_secs ))
            while :; do
              info="$("$herdr_bin" agent list 2>/dev/null || true)"
              state="$(printf '%s' "$info" | jq -r --arg p "$pane" 'first(.result.agents[]? | select(.pane_id == $p) | .agent_status) // empty' 2>/dev/null || true)"
              agent_sid="$(printf '%s' "$info" | jq -r --arg p "$pane" 'first(.result.agents[]? | select(.pane_id == $p) | .agent_session.value) // empty' 2>/dev/null || true)"
              case "$state" in
                idle|done) break ;;
                blocked)
                  fail "tab $tab pane $pane is blocked; read it with 'herdr agent read $pane --source recent-unwrapped' and let the person answer"
                  ;;
              esac
              if [ "$(date +%s)" -ge "$deadline" ]; then
                fail "tab $tab pane $pane did not reach an agent prompt in ''${wait_secs}s; inspect it before prompting"
              fi
              sleep 0.5
            done
          fi
          if [ "$json" = 1 ]; then
            jq -nc \
              --arg task "$task" \
              --arg task_id "$seat_task" \
              --arg dir "$(realpath -m "$dir")" \
              --arg label "$task" \
              --arg ws "$ws" \
              --arg tab "$tab" \
              --arg pane "$pane" \
              --arg state "$state" \
              --arg sid "$agent_sid" \
              '{task:$task,task_id:$task_id,dir:$dir,label:$label,herdr_workspace:$ws,herdr_tab:$tab,herdr_pane:$pane,agent_status:$state,agent_session:$sid}'
          fi
          exit 0
          ;;
        tab)
          new_tab=1
          shift
          ;;
        *)
          new_tab=0
          ;;
      esac

      task="''${1:-}"
      [ -n "$task" ] || usage
      shift || true
      [ "''${1:-}" = "--" ] && shift
      valid_task "$task" || { echo "rookery: invalid task '$task' (use [A-Za-z0-9._-])" >&2; exit 2; }

      dir="$(ws_dir_for "$task")"
      if workspace_exists "$task"; then
        echo "rookery: workspace '$task' already exists - 'rookery rm $task' to recreate it" >&2
        exit 1
      fi
      [ -e "$dir" ] && { echo "rookery: path already exists: $dir" >&2; exit 1; }

      # Maintenance is skipped when an interactive seat asks for a workspace: a
      # full classification plus a network fetch costs seconds, and the seat must
      # appear on the keypress. The daily timer owns reclamation and `jjwork`
      # fetches before any real work.
      if [ "''${ROOKERY_NO_MAINT:-0}" != "1" ]; then
        # Opportunistically reclaim finished workspaces before adding a new one.
        # Best-effort: a gc failure must never block workspace creation. Skips
        # the fetch (do_fetch=0) because the create path fetches below.
        rookery_gc 0 || true
      fi

      run_hook pre_create "$task" "$dir" "''${ROOKERY_HOOK_PRE_CREATE:-}"

      if [ "''${ROOKERY_NO_MAINT:-0}" != "1" ]; then
        jj git fetch >/dev/null 2>&1 || true
      fi
      jj workspace add "$dir" --name "$task" -r "$trunk" -m "$task: started from $trunk"

      # A seat started with `herdr --task` joins its task as soon as the
      # workspace exists. A store failure leaves the workspace unassigned
      # rather than losing it; the board still lists it as its own row.
      if [ -n "''${ROOKERY_TASK_ASSIGN:-}" ]; then
        store_assign "''${ROOKERY_TASK_ASSIGN}" "$task" \
          || echo "rookery: could not assign '$task' to task '$ROOKERY_TASK_ASSIGN'" >&2
      fi

      run_hook post_create "$task" "$dir" "''${ROOKERY_HOOK_POST_CREATE:-}"
      approve_envrc "$dir"

      # Herdr tab labels are the navigation key (goto picker, expanded sidebar).
      # Name the seat after the task so a new tab reads as the task, not as a
      # generic shell. Guard-silent: HERDR_TAB_ID is unset outside a herdr pane,
      # and a failed rename must never block the launch.
      if [ -n "''${HERDR_TAB_ID:-}" ] && command -v herdr >/dev/null 2>&1; then
        herdr tab rename "$HERDR_TAB_ID" "$task" >/dev/null 2>&1 || true
      fi

      if [ "''${ROOKERY_V2:-0}" = "1" ]; then
        # Both names resolve to the same launcher, which pins the config root and
        # its database itself; the flag is kept for existing invocations.
        run=(opencode2 "$@")
      else
        run=(opencode "$@")
      fi

      if [ "$new_tab" = "1" ]; then
        if [ -z "''${ZELLIJ_SESSION_NAME:-}" ]; then
          echo "rookery: 'tab' must run inside a zellij session" >&2
          exit 1
        fi
        zellij action new-tab --name "$task" --cwd "$dir" -- "''${run[@]}"
      else
        cd "$dir"
        exec "''${run[@]}"
      fi
    '';
    meta = {
      description = "A rookery of jj workspaces for coding agents";
      homepage = "https://github.com/FelixSchausberger/rookery";
      license = lib.licenses.mit;
      platforms = lib.platforms.linux;
      mainProgram = "rookery";
    };
  };
in
  symlinkJoin {
    name = "rookery";
    paths = [command];
    # The pre-rename command stays for one release: scripts, keybindings and the
    # agent plugin may still call `ocws`.
    postBuild = "ln -s rookery $out/bin/ocws";
    meta = command.meta // {mainProgram = "rookery";};
  }
