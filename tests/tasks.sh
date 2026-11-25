#!/usr/bin/env bash
# Fixture test for the Rookery task layer: the JSON board, the task store and
# its maintenance paths, cleanup marks, seat focus and task seats. Runs the
# command on PATH against a throwaway jj repository with a local git origin and
# a fake herdr; nothing touches the real fleet, cache or base.
set -euo pipefail

fail() {
	printf 'FAIL: %s\n' "$*" >&2
	exit 1
}

command -v rookery >/dev/null 2>&1 || fail "rookery not on PATH"
command -v jq >/dev/null 2>&1 || fail "jq not on PATH"
command -v jj >/dev/null 2>&1 || fail "jj not on PATH"
command -v sqlite3 >/dev/null 2>&1 || fail "sqlite3 not on PATH"

work=$(mktemp -d)
trap 'chmod -R u+w "$work" 2>/dev/null; rm -rf "$work"' EXIT
export HOME="$work/home"
export XDG_CACHE_HOME="$work/cache"
export XDG_DATA_HOME="$work/data"
export ROOKERY_BASE="$work/base"
ws_prefix="${ROOKERY_WS_PREFIX:-ws-}"
mkdir -p "$HOME" "$XDG_CACHE_HOME" "$XDG_DATA_HOME" "$ROOKERY_BASE" "$work/bin"

# A fake herdr makes the seat paths testable without a terminal: tab creation
# reports stable ids, pane run executes the command the way a real pane would,
# and focus is recorded.
bash_bin="$(command -v bash)"
cat >"$work/bin/herdr" <<EOF
#!$bash_bin
set -euo pipefail
log="\${FAKE_HERDR_LOG:-$work/herdr.log}"
case "\${1:-}" in
  agent)
    [ "\${2:-}" = "list" ] || exit 2
    printf '{"result":{"agents":[]}}\\n'
    ;;
  pane)
    case "\${2:-}" in
      list)
        printf '{"result":{"panes":[{"pane_id":"w1:p1","tab_id":"w1:t1","cwd":"%s","foreground_cwd":"%s","agent_status":"idle"}]}}\\n' "\${FAKE_PANE_CWD:-/nonexistent}" "\${FAKE_PANE_CWD:-/nonexistent}"
        ;;
      run)
        printf 'run %s\\n' "\${4:-}" >>"\$log"
        "$bash_bin" -c "\${4:-true}"
        ;;
      *) exit 2 ;;
    esac
    ;;
  tab)
    case "\${2:-}" in
      create) printf '{"result":{"root_pane":{"pane_id":"w1:p9"},"tab":{"tab_id":"w1:t9","workspace_id":"w1"}}}\\n' ;;
      focus) printf 'focus %s\\n' "\${3:-}" >>"\$log" ;;
      rename) exit 0 ;;
      *) exit 2 ;;
    esac
    ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$work/bin/herdr"
printf '#!%s\nexit 0\n' "$(command -v bash)" >"$work/bin/opencode"
chmod +x "$work/bin/opencode"
export PATH="$work/bin:$PATH"
export HERDR_BIN_PATH="$work/bin/herdr"
export FAKE_HERDR_LOG="$work/herdr.log"
: >"$FAKE_HERDR_LOG"

# A git origin with a main branch, fetched into a colocated jj repository.
git init -q "$work/origin"
(
	cd "$work/origin"
	echo base >file.txt
	git add file.txt
	git -c user.name=t -c user.email=t@t commit -q -m base
	git branch -M main
)
mkdir "$work/repo"
cd "$work/repo"
jj git init --colocate . >/dev/null 2>&1
jj config set --repo user.name "rookery test" >/dev/null 2>&1
jj config set --repo user.email "rookery@example.invalid" >/dev/null 2>&1
git remote add origin "$work/origin"
jj git fetch >/dev/null 2>&1
jj bookmark track main@origin >/dev/null 2>&1

jj workspace add "$ROOKERY_BASE/${ws_prefix}alpha" --name alpha -r main@origin -m "alpha: started from main@origin" >/dev/null 2>&1
jj workspace add "$ROOKERY_BASE/${ws_prefix}beta" --name beta -r main@origin -m "beta: started from main@origin" >/dev/null 2>&1

# `help` is a word, not a task: it prints usage and creates nothing.
before=$(jj --ignore-working-copy workspace list -T 'name ++ "\n"' | wc -l)
out=$(rookery help)
after=$(jj --ignore-working-copy workspace list -T 'name ++ "\n"' | wc -l)
[ "$before" -eq "$after" ] || fail "rookery help created a workspace"
printf '%s' "$out" | grep -Fq "Usage:" || fail "rookery help did not print usage: $out"

# A recent session gives the JSON board a conversation title.
mkdir -p "$XDG_DATA_HOME/opencode"
sqlite3 "$XDG_DATA_HOME/opencode/opencode.db" \
	"create table session_v2 (directory text, title text, time_archived integer, time_updated integer);
	insert into session_v2 values ('$ROOKERY_BASE/${ws_prefix}alpha', 'Board conversation', null, $(($(date +%s) * 1000)));"

json_ok() {
	printf '%s\n' "$1" | while IFS= read -r line; do
		[ -n "$line" ] || continue
		printf '%s' "$line" | jq -e . >/dev/null || fail "invalid JSON line: $line"
	done
}

# --- the JSON board ------------------------------------------------------
board=$(rookery ls --json --refresh)
json_ok "$board"
[ "$(printf '%s\n' "$board" | wc -l)" -eq 3 ] || fail "json board is not one line per workspace: $board"
printf '%s\n' "$board" | jq -e 'select(.workspace == "alpha")' >/dev/null 2>&1 || fail "alpha missing from the json board"
alpha=$(printf '%s\n' "$board" | jq -c 'select(.workspace == "alpha")')
[ "$(printf '%s' "$alpha" | jq -r '.task')" = "null" ] || fail "unassigned workspace has a task: $alpha"
[ "$(printf '%s' "$alpha" | jq -r '.state')" = "empty" ] || fail "alpha state is not empty: $alpha"
[ "$(printf '%s' "$alpha" | jq -r '.session.title')" = "Board conversation" ] || fail "session title missing: $alpha"
[ "$(printf '%s' "$alpha" | jq -r '.wait')" = "null" ] || fail "wait is not null before parking: $alpha"
[ "$(printf '%s' "$alpha" | jq -r '.clean')" = "null" ] || fail "clean is not null before marking: $alpha"
[ "$(printf '%s' "$alpha" | jq -r '.age | type')" = "number" ] || fail "age is not a number: $alpha"
# The count contract is unchanged by the new mode.
[ "$(rookery ls --counts --refresh)" = "2 2 0 0 0 0 1 0" ] || fail "counts changed: $(rookery ls --counts --refresh)"

rookery wait alpha "blocked on review" >/dev/null
alpha=$(rookery ls --json | jq -c 'select(.workspace == "alpha")')
[ "$(printf '%s' "$alpha" | jq -r '.wait.reason')" = "blocked on review" ] || fail "wait reason missing: $alpha"
[ "$(printf '%s' "$alpha" | jq -r '.wait.age | type')" = "number" ] || fail "wait age is not a number: $alpha"
rookery go alpha >/dev/null
rookery clean alpha >/dev/null
alpha=$(rookery ls --json | jq -c 'select(.workspace == "alpha")')
[ "$(printf '%s' "$alpha" | jq -r '.clean.age | type')" = "number" ] || fail "clean mark missing from json: $alpha"
rookery clean --unmark alpha >/dev/null

# --- the task store ------------------------------------------------------
[ ! -f "$ROOKERY_BASE/tasks.json" ] || fail "a read created the task store"
[ -z "$(rookery task list)" ] || fail "fresh store lists tasks"
rookery task add "Board task" >/dev/null
rookery task add "Board task" >/dev/null
tasks=$(rookery task list --json)
json_ok "$tasks"
[ "$(printf '%s\n' "$tasks" | jq -s 'length')" -eq 2 ] || fail "duplicate title did not suffix: $tasks"
printf '%s\n' "$tasks" | jq -e 'select(.id == "board-task" and .status == "open" and (.workspaces | length) == 0)' >/dev/null 2>&1 || fail "board-task shape is wrong: $tasks"
printf '%s\n' "$tasks" | jq -e 'select(.id == "board-task-2")' >/dev/null 2>&1 || fail "second id is not suffixed: $tasks"
[ "$(jq -r '.tasks | length' "$ROOKERY_BASE/tasks.json")" -eq 2 ] || fail "store document is not the task list"

rookery task assign board-task alpha >/dev/null
[ "$(rookery ls --json | jq -r 'select(.workspace == "alpha") | .task')" = "board-task" ] || fail "assignment is not on the board"
if rookery task assign board-task-2 alpha 2>/dev/null; then fail "reassigning without --move succeeded"; fi
rookery task assign --move board-task-2 alpha >/dev/null
[ "$(rookery ls --json | jq -r 'select(.workspace == "alpha") | .task')" = "board-task-2" ] || fail "--move did not move the workspace"
if rookery task assign board-task missing 2>/dev/null; then fail "assigning a missing workspace succeeded"; fi
if rookery task assign missing alpha --move 2>/dev/null; then fail "assigning to a missing task succeeded"; fi
if rookery task assign board-task default 2>/dev/null; then fail "assigning the primary checkout succeeded"; fi

# A claim renames the workspace; the grouping must follow it.
jj workspace add "$ROOKERY_BASE/${ws_prefix}t-1234" --name t-1234 -r main@origin -m "t-1234: started from main@origin" >/dev/null 2>&1
rookery task assign board-task t-1234 >/dev/null
(
	cd "$ROOKERY_BASE/${ws_prefix}t-1234"
	rookery title "Renamed seat" >/dev/null
)
jj --ignore-working-copy workspace list -T 'name ++ "\n"' | grep -qx "renamed-seat" || fail "title did not rename the workspace"
[ "$(jq -r '.tasks[] | select(.id == "board-task") | .workspaces[]' "$ROOKERY_BASE/tasks.json")" = "renamed-seat" ] || fail "the store kept the pre-rename name"

# Removal drops the row; the marker paths share rip_workspace.
rookery rm --force renamed-seat >/dev/null
if jq -e '.tasks[] | select(.workspaces | index("renamed-seat"))' "$ROOKERY_BASE/tasks.json" >/dev/null 2>&1; then
	fail "the store kept a removed workspace"
fi

# Concurrent writers serialize on the lock and never leave invalid JSON.
for i in 1 2 3 4 5 6 7 8; do
	rookery task add "Parallel $i" >/dev/null &
done
wait
jq -e . "$ROOKERY_BASE/tasks.json" >/dev/null || fail "concurrent adds corrupted the store"
[ "$(rookery task list --json | jq -s '[.[] | select(.id | startswith("parallel"))] | length')" -eq 8 ] || fail "concurrent adds lost tasks"

# Archive only an empty task; done only a task with workspaces.
rookery task add "Archivable" >/dev/null
rookery task archive archivable >/dev/null
printf '%s\n' "$(rookery task list --json)" | jq -e 'select(.id == "archivable" and .status == "archived")' >/dev/null 2>&1 || fail "archive did not mark the task"
if rookery task archive board-task-2 2>/dev/null; then fail "archiving a task with workspaces succeeded"; fi
if rookery task "done" archivable 2>/dev/null; then fail "task done on an empty task succeeded"; fi
rookery task "done" board-task-2 >/dev/null
[ -f "$ROOKERY_BASE/alpha.clean" ] || fail "task done did not mark the workspace"
[ ! -f "$ROOKERY_BASE/beta.clean" ] || fail "task done marked a workspace outside the task"

# --- cleanup -------------------------------------------------------------
# beta is not part of board-task-2; mark it and give it work that forbids
# removal.
rookery clean beta >/dev/null
(
	cd "$ROOKERY_BASE/${ws_prefix}beta"
	echo beta >b.txt
	jj describe -m "beta: work" >/dev/null 2>&1
)
out=$(rookery clean --apply)
printf '%s' "$out" | grep -Fq "keeping 'beta' (unpushed)" || fail "clean --apply removed unpushed work: $out"
[ -d "$ROOKERY_BASE/${ws_prefix}beta" ] || fail "clean --apply removed beta"
[ -d "$ROOKERY_BASE/${ws_prefix}alpha" ] && fail "clean --apply kept a marked, merged workspace"
printf '%s\n' "$(rookery task list --json)" | jq -e 'select(.id == "board-task-2" and .status == "archived")' >/dev/null 2>&1 || fail "a done task whose last workspace was removed did not archive"

# A live process also protects a marked empty workspace.
jj workspace add "$ROOKERY_BASE/${ws_prefix}gamma" --name gamma -r main@origin -m "gamma: started from main@origin" >/dev/null 2>&1
rookery clean gamma >/dev/null
(
	cd "$ROOKERY_BASE/${ws_prefix}gamma"
	exec sleep 30
) &
sleeper=$!
sleep 0.3
out=$(rookery clean --apply)
printf '%s' "$out" | grep -Fq "keeping 'gamma' (live process)" || fail "clean --apply ignored a live process: $out"
kill "$sleeper" 2>/dev/null || true
wait "$sleeper" 2>/dev/null || true
out=$(rookery clean --apply)
printf '%s' "$out" | grep -Fq "removed 'gamma'" || fail "clean --apply did not remove the idle workspace: $out"
[ ! -d "$ROOKERY_BASE/${ws_prefix}gamma" ] || fail "gamma survived the sweep"
[ ! -f "$ROOKERY_BASE/gamma.clean" ] || fail "the clean marker survived removal"

# A parked workspace outranks the cleanup mark, like every gc gate.
jj workspace add "$ROOKERY_BASE/${ws_prefix}delta" --name delta -r main@origin -m "delta: started from main@origin" >/dev/null 2>&1
rookery wait delta "external blocker" >/dev/null
rookery clean delta >/dev/null
out=$(rookery clean --apply)
printf '%s' "$out" | grep -Fq "keeping 'delta' (parked)" || fail "clean --apply ignored the wait marker: $out"
[ -d "$ROOKERY_BASE/${ws_prefix}delta" ] || fail "clean --apply removed a parked workspace"
rookery go delta >/dev/null
out=$(rookery clean --apply)
printf '%s' "$out" | grep -Fq "removed 'delta'" || fail "clean --apply did not remove delta after go: $out"

# --current resolves from inside the workspace and refuses the primary.
(
	cd "$ROOKERY_BASE/${ws_prefix}beta"
	rookery clean --current >/dev/null
	[ -f "$ROOKERY_BASE/beta.clean" ] || fail "clean --current did not mark beta"
	rookery clean --unmark --current >/dev/null
	[ ! -f "$ROOKERY_BASE/beta.clean" ] || fail "clean --unmark --current kept the marker"
)
if (cd "$work/repo" && rookery clean --current 2>/dev/null); then fail "clean --current accepted the primary checkout"; fi

# --- seats ---------------------------------------------------------------
export FAKE_PANE_CWD="$ROOKERY_BASE/${ws_prefix}beta"
rookery open beta | grep -Fq "opened 'beta'" || fail "open did not report the workspace"
grep -Fqx "focus w1:t1" "$FAKE_HERDR_LOG" || fail "open did not focus the pane's tab"
rookery task assign board-task beta >/dev/null
rookery open board-task | grep -Fq "opened 'beta'" || fail "open by task did not resolve beta"
if rookery open missing 2>/dev/null; then fail "open accepted an unknown name"; fi
if rookery open default 2>/dev/null; then fail "open accepted the primary checkout"; fi

# herdr --task starts a new workspace and assigns it.
seat=$(rookery herdr --task board-task --json)
[ "$(printf '%s' "$seat" | jq -r '.task_id')" = "board-task" ] || fail "herdr --task did not report the task: $seat"
ws=$(printf '%s' "$seat" | jq -r '.task')
[ "$ws" = "board-task" ] || fail "herdr --task picked an unexpected workspace name: $seat"
jj --ignore-working-copy workspace list -T 'name ++ "\n"' | grep -qx "board-task" || fail "herdr --task did not create the workspace"
printf '%s\n' "$(jq -r '.tasks[] | select(.id == "board-task") | .workspaces[]' "$ROOKERY_BASE/tasks.json")" | grep -qx "board-task" || fail "herdr --task did not assign the workspace"
seat2=$(rookery herdr --task board-task --json)
[ "$(printf '%s' "$seat2" | jq -r '.task')" = "board-task-2" ] || fail "a second seat did not suffix: $seat2"
if rookery herdr --task missing 2>/dev/null; then fail "herdr --task accepted an unknown task"; fi
if rookery herdr --task archivable 2>/dev/null; then fail "herdr --task accepted an archived task"; fi

# --- task lifecycle ------------------------------------------------------
# An open task holds its workspaces out of gc and out of the cleanup sweep,
# even after a worker marks itself. Only `task done` by an ungrouped caller
# releases them, and the last removal archives the task.
jj workspace add "$ROOKERY_BASE/${ws_prefix}epsilon" --name epsilon -r main@origin -m "epsilon: started from main@origin" >/dev/null 2>&1
rookery task add --id lifecycle "Lifecycle" >/dev/null
rookery task assign lifecycle epsilon >/dev/null
rookery task note lifecycle "decision: keep the daemon" >/dev/null
printf '%s\n' "$(rookery task list --json)" | jq -e 'select(.id == "lifecycle" and (.notes | contains("decision: keep the daemon")))' >/dev/null 2>&1 || fail "task note did not persist"
if (cd "$ROOKERY_BASE/${ws_prefix}epsilon" && rookery task "done" lifecycle 2>/dev/null); then
	fail "a grouped worker finished its own task"
fi
rookery clean epsilon >/dev/null
out=$(rookery clean --apply)
printf '%s' "$out" | grep -Fq "keeping 'epsilon' (task 'lifecycle' open)" || fail "clean --apply ignored the open task: $out"
[ -d "$ROOKERY_BASE/${ws_prefix}epsilon" ] || fail "clean --apply removed an open task workspace"
touch -d "2 days ago" "$ROOKERY_BASE/${ws_prefix}epsilon"
out=$(rookery gc)
printf '%s' "$out" | grep -Fq "keeping 'epsilon' (task 'lifecycle' open)" || fail "gc reclaimed an open task workspace: $out"
[ -d "$ROOKERY_BASE/${ws_prefix}epsilon" ] || fail "gc removed an open task workspace"
tmp=$(mktemp)
jq '.tasks |= map(if .id == "lifecycle" then .updated = 1 else . end)' "$ROOKERY_BASE/tasks.json" >"$tmp"
mv "$tmp" "$ROOKERY_BASE/tasks.json"
out=$(rookery gc)
printf '%s' "$out" | grep -Fq "task 'lifecycle' has been open" || fail "gc did not nudge a stale open task: $out"
rookery task "done" lifecycle >/dev/null
printf '%s\n' "$(rookery task list --json)" | jq -e 'select(.id == "lifecycle" and .status == "done")' >/dev/null 2>&1 || fail "task done did not set the status"
out=$(rookery clean --apply)
printf '%s' "$out" | grep -Fq "removed 'epsilon'" || fail "clean --apply did not release a done task workspace: $out"
printf '%s\n' "$(rookery task list --json)" | jq -e 'select(.id == "lifecycle" and .status == "archived")' >/dev/null 2>&1 || fail "the last workspace removal did not archive the task"

# A done task with real work stays visible: clean --apply keeps the workspace
# and archive refuses until the work is resolved or deliberately discarded.
jj workspace add "$ROOKERY_BASE/${ws_prefix}zeta" --name zeta -r main@origin -m "zeta: started from main@origin" >/dev/null 2>&1
rookery task add --id lifecycle-work "Lifecycle work" >/dev/null
rookery task assign lifecycle-work zeta >/dev/null
(
	cd "$ROOKERY_BASE/${ws_prefix}zeta"
	echo work >z.txt
	jj describe -m "zeta: work" >/dev/null 2>&1
)
rookery task "done" lifecycle-work >/dev/null
out=$(rookery clean --apply)
printf '%s' "$out" | grep -Fq "keeping 'zeta' (unpushed)" || fail "clean --apply removed a done task's unpushed work: $out"
if rookery task archive lifecycle-work 2>/dev/null; then fail "archive accepted a done task with a workspace"; fi
rookery rm --force zeta >/dev/null
printf '%s\n' "$(rookery task list --json)" | jq -e 'select(.id == "lifecycle-work" and .status == "archived")' >/dev/null 2>&1 || fail "discarding the last workspace did not archive the done task"

# --- parent tasks --------------------------------------------------------
rookery task add --id epic "Epic" >/dev/null
rookery task add --id leaf --parent epic "Leaf" >/dev/null
printf '%s\n' "$(rookery task list --json)" | jq -e 'select(.id == "leaf" and .parent == "epic")' >/dev/null 2>&1 || fail "parent was not stored"
printf '%s\n' "$(rookery task list --json)" | jq -e 'select(.id == "epic" and (has("parent") | not))' >/dev/null 2>&1 || fail "a root task grew a parent field"
rookery task list | grep -Eq '^    leaf +open' || fail "task list did not indent the child"
[ "$(jq -r '.version' "$ROOKERY_BASE/tasks.json")" = "2" ] || fail "store version was not upgraded"
if rookery task add --id deep --parent leaf "Deep" 2>/dev/null; then fail "a two-level parent link was accepted"; fi
if rookery task parent epic leaf 2>/dev/null; then fail "a parent was made a child of its child"; fi
if rookery task parent leaf leaf 2>/dev/null; then fail "a task was made its own parent"; fi
if rookery task parent leaf missing 2>/dev/null; then fail "a missing parent was accepted"; fi
if rookery task archive epic 2>/dev/null; then fail "archiving a parent with an open child succeeded"; fi
rookery task parent --clear leaf >/dev/null
printf '%s\n' "$(rookery task list --json)" | jq -e 'select(.id == "leaf" and (has("parent") | not))' >/dev/null 2>&1 || fail "--clear did not make a root"
rookery task parent leaf epic >/dev/null
jj workspace add "$ROOKERY_BASE/${ws_prefix}parent-seat" --name parent-seat -r main@origin -m "parent-seat: started from main@origin" >/dev/null 2>&1
rookery task assign leaf parent-seat >/dev/null
if rookery task done epic 2>/dev/null; then fail "task done ignored an open child"; fi
rookery task done --recursive epic >/dev/null
[ -f "$ROOKERY_BASE/parent-seat.clean" ] || fail "recursive task done missed the child workspace"
printf '%s\n' "$(rookery task list --json)" | jq -e 'select(.id == "leaf" and .status == "done")' >/dev/null 2>&1 || fail "recursive task done did not set the child done"
if rookery task archive epic 2>/dev/null; then fail "archiving a lead over a done child succeeded"; fi
rookery clean --apply >/dev/null
printf '%s\n' "$(rookery task list --json)" | jq -e 'select(.id == "leaf" and .status == "archived")' >/dev/null 2>&1 || fail "the child did not archive after its workspace went"

# A corrupt store is reported by its owners and never rewritten by a read.
jj workspace add "$ROOKERY_BASE/${ws_prefix}eta" --name eta -r main@origin -m "eta: started from main@origin" >/dev/null 2>&1
printf 'garbage\n' >"$ROOKERY_BASE/tasks.json"
out=$(rookery gc 2>&1)
printf '%s' "$out" | grep -Fq "task store is unreadable" || fail "gc ignored an unreadable task store: $out"
[ -d "$ROOKERY_BASE/${ws_prefix}eta" ] || fail "gc removed a workspace with an unreadable task store"
if rookery task list 2>/dev/null; then fail "task list accepted a corrupt store"; fi
rookery ls --json >/dev/null || fail "the JSON board failed on a corrupt store"
grep -qx 'garbage' "$ROOKERY_BASE/tasks.json" || fail "a read rewrote the corrupt store"

printf 'Rookery task fixture: ok\n'
