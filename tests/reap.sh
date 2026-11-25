#!/usr/bin/env bash
# Fixture test for the session reaper: the default pass prunes old child
# subagent transcripts and keeps every top-level conversation, a recent root or
# a recent descendant keeps its subtree, `--roots` is the explicit pass for a
# dead top-level conversation, and a failed export keeps the whole tree. The
# fake opencode records every export and delete, so the export-before-delete
# order is observable.
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
export ROOKERY_SESSION_ARCHIVE="$work/archive"
export ROOKERY_REAP_CHILD_AGE=3600
export FAKE_OPENCODE_LOG="$work/opencode.log"
mkdir -p "$HOME" "$XDG_CACHE_HOME" "$XDG_DATA_HOME" "$ROOKERY_BASE" "$work/bin"
: >"$FAKE_OPENCODE_LOG"

bash_bin="$(command -v bash)"
cat >"$work/bin/opencode" <<EOF
#!$bash_bin
set -euo pipefail
log="\${FAKE_OPENCODE_LOG:?}"
case "\${1:-} \${2:-}" in
  "session export")
    id="\${3:-}"
    printf 'export %s\n' "\$id" >>"\$log"
    if [ "\$id" = "ses_fail_grandchild" ]; then
      echo "export refused" >&2
      exit 1
    fi
    printf '{"id":"%s"}\n' "\$id"
    ;;
  "session delete")
    printf 'delete %s\n' "\${3:-}" >>"\$log"
    ;;
  *) exit 2 ;;
esac
EOF
chmod +x "$work/bin/opencode"
export PATH="$work/bin:$PATH"

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
jj workspace add "$ROOKERY_BASE/ws-alpha" --name alpha -r main@origin -m "alpha: started from main@origin" >/dev/null 2>&1

now_ms=$(($(date +%s) * 1000))
old_ms=$((now_ms - 7200000))
mkdir -p "$XDG_DATA_HOME/opencode"
sqlite3 "$XDG_DATA_HOME/opencode/opencode.db" \
	"create table session_v2 (
	   id text primary key,
	   parent_id text,
	   directory text,
	   title text,
	   slug text,
	   time_updated integer,
	   time_archived integer
	 );
	 insert into session_v2 values
	   ('ses_dead', null, '$ROOKERY_BASE/ws-dead', 'Dead conversation', 'dead', $old_ms, null),
	   ('ses_dead_child', 'ses_dead', '$ROOKERY_BASE/ws-dead', 'Dead child', 'dead-child', $old_ms, null),
	   ('ses_dead_grandchild', 'ses_dead_child', '$ROOKERY_BASE/ws-dead', null, 'dead-grandchild', $old_ms, null),
	   ('ses_live_new', null, '$ROOKERY_BASE/ws-alpha', 'New live conversation', 'live-new', $now_ms, null),
	   ('ses_live_new_child', 'ses_live_new', '$ROOKERY_BASE/ws-alpha', null, 'live-new-child', $old_ms, null),
	   ('ses_live_old', null, '$ROOKERY_BASE/ws-alpha', 'Old live conversation', 'live-old', $old_ms, null),
	   ('ses_live_old_child', 'ses_live_old', '$ROOKERY_BASE/ws-alpha', null, 'live-old-child', $old_ms, null),
	   ('ses_resumed', null, '$ROOKERY_BASE/ws-alpha', 'Resumed conversation', 'resumed', $old_ms, null),
	   ('ses_resumed_child', 'ses_resumed', '$ROOKERY_BASE/ws-alpha', null, 'resumed-child', $old_ms, null),
	   ('ses_resumed_grandchild', 'ses_resumed_child', '$ROOKERY_BASE/ws-alpha', null, 'resumed-grandchild', $now_ms, null),
	   ('ses_fail', null, '$ROOKERY_BASE/ws-alpha', 'Fail conversation', 'fail', $old_ms, null),
	   ('ses_fail_child', 'ses_fail', '$ROOKERY_BASE/ws-alpha', null, 'fail-child', $old_ms, null),
	   ('ses_fail_grandchild', 'ses_fail_child', '$ROOKERY_BASE/ws-alpha', null, 'fail-grandchild', $old_ms, null),
	   ('ses_archived', null, '$ROOKERY_BASE/ws-alpha', 'Archived conversation', 'archived', $old_ms, 1),
	   ('ses_archived_child', 'ses_archived', '$ROOKERY_BASE/ws-alpha', null, 'archived-child', $old_ms, null);"

# --- default dry run -----------------------------------------------------
out=$(rookery reap --dry-run 2>&1)
printf '%s' "$out" | grep -Fq "would export and delete child 'ses_dead_child'" || fail "dead child was not listed: $out"
printf '%s' "$out" | grep -Fq "would export and delete child 'ses_live_old_child'" || fail "old live child was not listed: $out"
printf '%s' "$out" | grep -Fq "would export and delete child 'ses_fail_child'" || fail "failing child was not listed: $out"
printf '%s' "$out" | grep -Fq "ses_live_new_child" && fail "a recent root's child was listed: $out"
printf '%s' "$out" | grep -Fq "ses_resumed_grandchild" && fail "a recent descendant was listed: $out"
printf '%s' "$out" | grep -Fq "ses_archived_child" && fail "an archived tree was listed: $out"
printf '%s' "$out" | grep -Fq "would export and delete 'ses_dead'" && fail "a top-level conversation was listed by default: $out"
[ ! -s "$FAKE_OPENCODE_LOG" ] || fail "dry run invoked the agent: $(cat "$FAKE_OPENCODE_LOG")"
[ ! -e "$ROOKERY_SESSION_ARCHIVE" ] || fail "dry run created the archive"

# --- the default pass ----------------------------------------------------
out=$(rookery reap 2>&1)
printf '%s' "$out" | grep -Fq "exported and deleted child 'ses_dead_child'" || fail "dead child was not pruned: $out"
printf '%s' "$out" | grep -Fq "exported and deleted child 'ses_live_old_child'" || fail "old live child was not pruned: $out"
printf '%s' "$out" | grep -Fq "keeping child 'ses_fail_child'" || fail "failed export did not keep its tree: $out"
printf '%s' "$out" | grep -Fq "2 conversations exported and deleted, 1 kept; 5 top-level conversations kept" || fail "summary is wrong: $out"
printf '%s' "$out" | grep -Fq "exported and deleted 'ses_dead'" && fail "a top-level conversation was deleted by default: $out"

grep -Fqx "export ses_dead_child" "$FAKE_OPENCODE_LOG" || fail "child export missing"
grep -Fqx "export ses_dead_grandchild" "$FAKE_OPENCODE_LOG" || fail "grandchild export missing"
grep -Fqx "delete ses_dead_child" "$FAKE_OPENCODE_LOG" || fail "child delete missing"
grep -Fqx "export ses_fail_child" "$FAKE_OPENCODE_LOG" || fail "failing tree child was not attempted"
grep -Fqx "export ses_fail_grandchild" "$FAKE_OPENCODE_LOG" || fail "failing tree grandchild was not attempted"
! grep -Fqx "delete ses_fail_child" "$FAKE_OPENCODE_LOG" || fail "a tree with a failed export was deleted"
! grep -Fqx "delete ses_live_new_child" "$FAKE_OPENCODE_LOG" || fail "a recent root's child was deleted"
! grep -Fqx "delete ses_resumed_child" "$FAKE_OPENCODE_LOG" || fail "a child with a recent descendant was deleted"
! grep -Fqx "delete ses_dead" "$FAKE_OPENCODE_LOG" || fail "a top-level conversation was deleted by default"

[ -f "$ROOKERY_SESSION_ARCHIVE/ses_dead/ses_dead_child-dead-child.json" ] || fail "child export missing from archive"
[ -f "$ROOKERY_SESSION_ARCHIVE/ses_dead/ses_dead_grandchild-dead-grandchild.json" ] || fail "grandchild export missing from archive"
[ ! -f "$ROOKERY_SESSION_ARCHIVE/ses_dead/ses_dead-dead.json" ] || fail "a top-level conversation was exported by default"
[ -f "$ROOKERY_SESSION_ARCHIVE/ses_live_old/ses_live_old_child-live-old-child.json" ] || fail "live child export missing from archive"
[ -f "$ROOKERY_SESSION_ARCHIVE/ses_fail/ses_fail_child-fail-child.json" ] || fail "the successful part of the failing tree was not archived"
[ ! -f "$ROOKERY_SESSION_ARCHIVE/ses_fail/ses_fail_grandchild-fail-grandchild.json" ] || fail "a failed export was archived"

# --- explicit root pass, dry run ----------------------------------------
out=$(rookery reap --roots --dry-run 2>&1)
printf '%s' "$out" | grep -Fq "would export and delete 'ses_dead' (Dead conversation)" || fail "dead root was not listed: $out"
printf '%s' "$out" | grep -Fq "would export and delete 'ses_live_old'" && fail "a live root was listed: $out"
printf '%s' "$out" | grep -Fq "would export and delete 'ses_archived'" && fail "an archived root was listed: $out"
[ ! -f "$ROOKERY_SESSION_ARCHIVE/ses_dead/ses_dead-dead.json" ] || fail "dry run exported a root"

# --- explicit root pass --------------------------------------------------
out=$(rookery reap --roots 2>&1)
printf '%s' "$out" | grep -Fq "exported and deleted 'ses_dead' (Dead conversation)" || fail "dead root was not deleted: $out"
printf '%s' "$out" | grep -Fq "1 conversations exported and deleted, 0 kept; 4 top-level conversations kept" || fail "root summary is wrong: $out"
grep -Fqx "export ses_dead" "$FAKE_OPENCODE_LOG" || fail "root export missing"
grep -Fqx "delete ses_dead" "$FAKE_OPENCODE_LOG" || fail "root delete missing"
! grep -Fqx "delete ses_live_new" "$FAKE_OPENCODE_LOG" || fail "a live root was deleted"
! grep -Fqx "delete ses_live_old" "$FAKE_OPENCODE_LOG" || fail "a live root was deleted"
! grep -Fqx "delete ses_archived" "$FAKE_OPENCODE_LOG" || fail "an archived root was deleted"
[ -f "$ROOKERY_SESSION_ARCHIVE/ses_dead/ses_dead-dead.json" ] || fail "root export missing from archive"

# --- a host without OpenCode --------------------------------------------
mv "$XDG_DATA_HOME/opencode/opencode.db" "$work/opencode.db"
out=$(rookery reap 2>&1)
printf '%s' "$out" | grep -Fq "no session database" || fail "missing database was not reported: $out"

printf 'Rookery reap fixture: ok\n'
