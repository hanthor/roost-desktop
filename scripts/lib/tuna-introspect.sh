# shellcheck shell=sh
# tuna-introspect.sh — semantic journey assertions over the shell's
# introspection snapshot (#65). Source after setting ARTIFACTS.
#
#   TUNA_INTROSPECT_FILE  exported: the shell writes its state here
#   assert_state ID EXPR DESC [TIMEOUT]
#       poll the snapshot until the jq boolean EXPR holds; record
#       "ID pass|fail DESC" in $ARTIFACTS/assertions.txt; fail fast.
#   keep_state NAME   copy the current snapshot to $ARTIFACTS/NAME.json
#
# A stage passes on *state*, never on pixels: a frame that changes while
# the state does not is a failure.

TUNA_INTROSPECT_FILE="$ARTIFACTS/shell-state.json"
export TUNA_INTROSPECT_FILE
ASSERTIONS="$ARTIFACTS/assertions.txt"
rm -f "$TUNA_INTROSPECT_FILE" "$ASSERTIONS"
: >"$ASSERTIONS"

command -v jq >/dev/null 2>&1 || {
    echo "tuna-introspect: jq is required" >&2
    exit 1
}

assert_state() {
    id="$1"
    expr="$2"
    desc="$3"
    timeout="${4:-20}"
    end=$(($(date +%s) + timeout))
    while [ "$(date +%s)" -le "$end" ]; do
        if [ -s "$TUNA_INTROSPECT_FILE" ] &&
            jq -e "$expr" "$TUNA_INTROSPECT_FILE" >/dev/null 2>&1; then
            echo "$id pass $desc" >>"$ASSERTIONS"
            echo "tuna-introspect: $id pass: $desc"
            return 0
        fi
        sleep 0.2
    done
    echo "$id fail $desc" >>"$ASSERTIONS"
    echo "tuna-introspect: $id FAIL: $desc (expr: $expr)" >&2
    if [ -s "$TUNA_INTROSPECT_FILE" ]; then
        echo "tuna-introspect: last state:" >&2
        jq . "$TUNA_INTROSPECT_FILE" >&2 || cat "$TUNA_INTROSPECT_FILE" >&2
    else
        echo "tuna-introspect: no state file was ever written" >&2
    fi
    return 1
}

keep_state() {
    if [ -s "$TUNA_INTROSPECT_FILE" ]; then
        cp "$TUNA_INTROSPECT_FILE" "$ARTIFACTS/$1.json"
    fi
}
