#!/bin/sh
# A stand-in for herdr-core's `hc`, enough for the daemon's integration test.
# Terminals are folders under $FAKE_HC_DIR/<name>/ holding `cwd`, `argv` and `log`;
# `rm` moves one to $FAKE_HC_DIR/.removed/<name>/.
# A "turn" (send --wait) answers at once: it writes "fake reply" to the outbox file the
# message names, the way a real bot following HERDR-BOTS.md would.
set -eu
dir=${FAKE_HC_DIR:?}
mkdir -p "$dir"
cmd=$1; shift
json=0; name=""; cwd=""; labels=""; rest=""
while [ $# -gt 0 ]; do
  case "$1" in
    --json) json=1 ;;
    --bg|--no-parent|--no-notify|--wait|--force) ;;
    --name) name=$2; shift ;;
    --cwd) cwd=$2; shift ;;
    --label) labels="$labels $2"; shift ;;
    --timeout|--lines|--if) shift ;;
    --) shift; if [ -z "$name" ]; then name=$1; shift; fi; rest="$*"; break ;;
    *) if [ -z "$name" ]; then name=$1; else rest="$rest $1"; fi ;;
  esac
  shift
done
t="$dir/$name"
case "$cmd" in
  run)
    mkdir -p "$t"; printf '%s' "$cwd" > "$t/cwd"; printf '%s' "$rest" > "$t/argv"; : > "$t/log"
    echo "{\"address\":\"t1\",\"name\":\"$name\",\"state\":\"live\",\"turn\":0}" ;;
  get)
    if [ -d "$t" ]; then
      echo "{\"address\":\"t1\",\"name\":\"$name\",\"state\":\"live\",\"status\":\"idle\",\"labels\":{},\"exit_code\":null}"
    else
      echo '{"error":{"kind":"not_found","message":"no terminal"}}' >&2; exit 3
    fi ;;
  wait)
    echo '{"ended":true,"reason":"idle","status":"idle","exit_code":null}' ;;
  send)
    text=$rest
    printf '%s\n' "$text" >> "$t/log"
    n=$(printf '%s' "$text" | sed -n 's/.*outbox\/\([0-9][0-9]*\)\.md.*/\1/p' | tail -n 1)
    # "ASKME" makes the fake bot ask its owner first, like a bot following HERDR-BOTS.md.
    if [ -n "$n" ] && printf '%s' "$text" | grep -q ASKME && ! printf '%s' "$text" | grep -q '\[answer\]'; then
      "${HERDR_BOTS_BIN:?}" needs-you "$n" "which env?" > /dev/null
    elif [ -n "$n" ]; then mkdir -p "$(cat "$t/cwd")/outbox"; echo "fake reply $n" > "$(cat "$t/cwd")/outbox/$n.md"; fi
    echo '{"started":1,"status":"working","turn":1,"wait":{"ended":true,"reason":"done","status":"done","exit_code":null}}' ;;
  read) cat "$t/log" 2>/dev/null || true ;;
  rm)
    mkdir -p "$dir/.removed"; rm -rf "$dir/.removed/$name"
    [ ! -d "$t" ] || mv "$t" "$dir/.removed/$name"
    echo '{"removed":"t1"}' ;;
  interrupt|kill) echo '{}' ;;
  *) echo "{\"error\":{\"kind\":\"unsupported\",\"message\":\"$cmd\"}}"; exit 2 ;;
esac
