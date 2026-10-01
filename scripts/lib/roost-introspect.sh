# shellcheck shell=sh
# roost-introspect.sh — semantic journey assertions over the shell's
# introspection snapshot (#65). Source after setting ARTIFACTS.
#
#   ROOST_INTROSPECT_FILE  exported: the shell writes its state here
#   assert_state ID EXPR DESC [TIMEOUT]
#       poll the snapshot until the jq boolean EXPR holds; record
#       "ID pass|fail DESC" in $ARTIFACTS/assertions.txt; fail fast.
#   keep_state NAME   copy the current snapshot to $ARTIFACTS/NAME.json
#
# A stage passes on *state*, never on pixels: a frame that changes while
# the state does not is a failure.

ROOST_INTROSPECT_FILE="$ARTIFACTS/shell-state.json"
export ROOST_INTROSPECT_FILE
ASSERTIONS="$ARTIFACTS/assertions.txt"
rm -f "$ROOST_INTROSPECT_FILE" "$ASSERTIONS"
: >"$ASSERTIONS"

command -v jq >/dev/null 2>&1 || {
    echo "roost-introspect: jq is required" >&2
    exit 1
}

assert_state() {
    id="$1"
    expr="$2"
    desc="$3"
    timeout="${4:-20}"
    end=$(($(date +%s) + timeout))
    while [ "$(date +%s)" -le "$end" ]; do
        if [ -s "$ROOST_INTROSPECT_FILE" ] &&
            jq -e "$expr" "$ROOST_INTROSPECT_FILE" >/dev/null 2>&1; then
            echo "$id pass $desc" >>"$ASSERTIONS"
            echo "roost-introspect: $id pass: $desc"
            return 0
        fi
        sleep 0.2
    done
    echo "$id fail $desc" >>"$ASSERTIONS"
    echo "roost-introspect: $id FAIL: $desc (expr: $expr)" >&2
    if [ -s "$ROOST_INTROSPECT_FILE" ]; then
        echo "roost-introspect: last state:" >&2
        jq . "$ROOST_INTROSPECT_FILE" >&2 || cat "$ROOST_INTROSPECT_FILE" >&2
    else
        echo "roost-introspect: no state file was ever written" >&2
    fi
    return 1
}

keep_state() {
    if [ -s "$ROOST_INTROSPECT_FILE" ]; then
        cp "$ROOST_INTROSPECT_FILE" "$ARTIFACTS/$1.json"
    fi
}
