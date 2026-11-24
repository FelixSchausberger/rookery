#!/usr/bin/env bash
# Parity harness: run the same scenario twice, once with the shell oracle and
# once with the candidate binary, and compare the normalized transcripts. The
# behavioral tests (board.sh, tasks.sh) own the semantics; this catches output
# drift between the two implementations while both are installed.
set -euo pipefail

oracle=${ROOKERY_ORACLE:-}
candidate=${ROOKERY_CANDIDATE:-}
[ -n "$oracle" ] || {
	echo "FAIL: ROOKERY_ORACLE is unset" >&2
	exit 1
}
[ -n "$candidate" ] || {
	echo "FAIL: ROOKERY_CANDIDATE is unset" >&2
	exit 1
}
command -v jj >/dev/null 2>&1 || {
	echo "FAIL: jj not on PATH" >&2
	exit 1
}
command -v git >/dev/null 2>&1 || {
	echo "FAIL: git not on PATH" >&2
	exit 1
}
command -v sqlite3 >/dev/null 2>&1 || {
	echo "FAIL: sqlite3 not on PATH" >&2
	exit 1
}
command -v jq >/dev/null 2>&1 || {
	echo "FAIL: jq not on PATH" >&2
	exit 1
}

work=$(mktemp -d)
trap 'chmod -R u+w "$work" 2>/dev/null; rm -rf "$work"' EXIT
mkdir -p "$work/oracle" "$work/candidate"

make_fake() {
	local bin=$1
	cat >"$bin/herdr" <<EOF
#!$(command -v bash)
set -euo pipefail
case "\${1:-}" in
  agent)
    case "\${2:-}" in
      list)
        if [ -n "\${FAKE_AGENT_SESSION:-}" ]; then
          printf '{"result":{"agents":[{"agent_session":{"value":"%s"},"workspace_id":"w1","tab_id":"w1:t1","pane_id":"w1:p1","agent_status":"idle"}]}}\\n' "\$FAKE_AGENT_SESSION"
        else
          printf '{"result":{"agents":[]}}\\n'
        fi
        ;;
      *) exit 2 ;;
    esac
    ;;
  pane)
    case "\${2:-}" in
      list)
        printf '{"result":{"panes":[{"pane_id":"w1:p1","tab_id":"w1:t1","cwd":"%s","foreground_cwd":"%s","agent_status":"idle"}]}}\\n' "\${FAKE_PANE_CWD:-/nonexistent}" "\${FAKE_PANE_CWD:-/nonexistent}"
        ;;
      run) "$(command -v bash)" -c "\${4:-true}" ;;
      *) exit 2 ;;
    esac
    ;;
  tab)
    case "\${2:-}" in
      create) printf '{"result":{"root_pane":{"pane_id":"w1:p9"},"tab":{"tab_id":"w1:t9","workspace_id":"w1"}}}\\n' ;;
      focus|rename) exit 0 ;;
      *) exit 2 ;;
    esac
    ;;
  *) exit 2 ;;
esac
EOF
	chmod +x "$bin/herdr"
	printf '#!%s\nexit 0\n' "$(command -v bash)" >"$bin/opencode"
	chmod +x "$bin/opencode"
}

record() {
	local work=$1 status
	shift
	{
		printf '###'
		printf ' %s' "$@"
		printf '\n'
	} >>"$work/transcript"
	if "$@" >"$work/stdout" 2>"$work/stderr"; then
		status=0
	else
		status=$?
	fi
	{
		printf 'EXIT=%s\n' "$status"
		cat "$work/stdout"
		cat "$work/stderr"
	} >>"$work/transcript"
}

scenario() {
	local bin=$1 work=$2
	mkdir -p "$work/home" "$work/cache" "$work/data" "$work/base" "$work/bin"
	export HOME="$work/home"
	export XDG_CACHE_HOME="$work/cache"
	export XDG_DATA_HOME="$work/data"
	export ROOKERY_BASE="$work/base"
	export ROOKERY_WS_PREFIX="ws-"
	export ROOKERY_TRUNK="main@origin"
	export PATH="$work/bin:$PATH"
	export HERDR_BIN_PATH="$work/bin/herdr"
	make_fake "$work/bin"

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
	jj config set --repo user.name "rookery parity" >/dev/null 2>&1
	jj config set --repo user.email "rookery@example.invalid" >/dev/null 2>&1
	git remote add origin "$work/origin"
	jj git fetch >/dev/null 2>&1
	jj bookmark track main@origin >/dev/null 2>&1

	mkdir -p "$XDG_DATA_HOME/opencode"
	sqlite3 "$XDG_DATA_HOME/opencode/opencode.db" \
		"create table session_v2 (directory text, title text, time_archived integer, time_updated integer);
		insert into session_v2 values ('$ROOKERY_BASE/ws-alpha', 'Parity conversation', null, $(($(date +%s) * 1000)));"

	jj workspace add "$ROOKERY_BASE/ws-alpha" --name alpha -r main@origin -m "alpha: started from main@origin" >/dev/null 2>&1
	jj workspace add "$ROOKERY_BASE/ws-beta" --name beta -r main@origin -m "beta: started from main@origin" >/dev/null 2>&1
	(
		cd "$ROOKERY_BASE/ws-alpha"
		echo alpha >a.txt
		jj describe -m "alpha: first" >/dev/null 2>&1
	)

	: >"$work/transcript"
	record "$work" "$bin" ls --counts --refresh
	record "$work" "$bin" ls --board --refresh
	record "$work" "$bin" ls --json --refresh
	record "$work" "$bin" task list --json
	record "$work" "$bin" task add "Parity task" --notes "one"
	record "$work" "$bin" task add "Parity task"
	record "$work" "$bin" task list --json
	record "$work" "$bin" task assign parity-task alpha
	record "$work" "$bin" ls --json --refresh
	record "$work" "$bin" wait alpha "blocked on parity"
	record "$work" "$bin" ls --json --refresh
	record "$work" "$bin" go alpha
	record "$work" "$bin" clean alpha
	record "$work" "$bin" ls --json --refresh
	record "$work" "$bin" clean --unmark alpha
	record "$work" "$bin" clean beta
	record "$work" "$bin" clean --apply
	record "$work" "$bin" task "done" parity-task
	record "$work" "$bin" clean --apply
	record "$work" "$bin" task archive parity-task-notes-one
	record "$work" "$bin" task list --json
	record "$work" "$bin" gc
	record "$work" "$bin" open alpha
	FAKE_PANE_CWD="$ROOKERY_BASE/ws-alpha"
	export FAKE_PANE_CWD
	record "$work" "$bin" open alpha
	(
		cd "$ROOKERY_BASE/ws-alpha"
		OPENCODE_SESSION_ID=ses_parity FAKE_AGENT_SESSION=ses_parity \
			"$bin" seat --json >"$work/stdout" 2>"$work/stderr" || true
	)
	{
		printf '### seat --json\n'
		cat "$work/stdout"
		cat "$work/stderr"
	} >>"$work/transcript"
	record "$work" "$bin" task add "Seat task"
	record "$work" "$bin" herdr --task seat-task --json
	record "$work" "$bin" ls --counts --refresh
	record "$work" "$bin" "done" --force alpha
	record "$work" "$bin" rm --force beta
	record "$work" "$bin" gc

	# Normalize everything the fixtures cannot make byte-identical: absolute
	# roots, clocks and commit ids.
	sed -e "s#$work#WORK#g" \
		-e "s#$bin#BIN#g" \
		-e 's/"age":-\?[0-9]\+/"age":N/g' \
		-e 's/"updated":-\?[0-9]\+/"updated":N/g' \
		-e 's/"created":-\?[0-9]\+/"created":N/g' \
		-e 's/\b[0-9a-f]\{40\}\b/CID/g' \
		-e 's/\b[0-9a-f]\{12\}\b/SHORT/g' \
		-e 's/^Working copy.*now at: .*/Working copy (@) now at: CID/' \
		-e 's/^Parent commit.*: .*/Parent commit (@-): CID/' \
		"$work/transcript" >"$work/normalized"
}

scenario "$oracle" "$work/oracle"
scenario "$candidate" "$work/candidate"

if ! diff -u "$work/oracle/normalized" "$work/candidate/normalized" >"$work/diff"; then
	cat "$work/diff" >&2
	echo "FAIL: oracle and candidate differ" >&2
	exit 1
fi

printf 'Rookery parity: ok\n'
