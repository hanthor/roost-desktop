#!/bin/sh
# Sourced by tuna-gtk-shell-proof, also used by its focused screenshot run.
# Requires that harness's private bus, GTK shell, AT-SPI and assertion helpers.
screenshot_keys_proof() {
# Drive the real exclusive-keyboard overlay and inspect the saved PNG,
# rather than treating a preference write or geometry helper as UI proof.
while IFS='|' read -r shot_keys shot_dims shot_position shot_label shot_pointer; do
    shot_workspace="$(jq -r '.active_workspace' "$CSTATE")"
    sleep 1.1 # GNOME filenames have one-second resolution.
    marker="$ARTIFACTS/shot-keys-$shot_label.marker"
    touch "$marker"
    xdotool key Print
    # GTK 4.14's AT-SPI root can return an unrealized hidden toplevel
    # when ChildCount/GetChildAtIndex straddle a map transition. Observe
    # the real mapped layer before walking its accessible tree.
    cstate 'any(.layers[]; .namespace == "tuna-screenshot-ui")' "screenshot overlay mapped" ||
        fail G-SCREENSHOT-KEYS "Print did not map selection for $shot_label"
    end=$(($(date +%s) + 20))
    until "$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/a11y-shot-keys-$shot_label.json" 5 >/dev/null 2>&1 &&
        showing "$ARTIFACTS/a11y-shot-keys-$shot_label.json" "Capture" || [ "$(date +%s)" -gt "$end" ]; do sleep 0.5; done
    showing "$ARTIFACTS/a11y-shot-keys-$shot_label.json" "Capture" ||
        fail G-SCREENSHOT-KEYS "Print did not open selection for $shot_label"
    xdotool key --clearmodifiers s r
    # Intentional word splitting: the fixture is a list of xdotool keys.
    # shellcheck disable=SC2086
    xdotool key --clearmodifiers $shot_keys
    # xdotool finishes sending X events before the nested Wayland client
    # and AT-SPI have necessarily processed the entire chord sequence.
    # Observe completion without replaying keys or accepting partial geometry.
    end=$(($(date +%s) + 10))
    shot_attempt=0
    while :; do
        shot_attempt=$((shot_attempt + 1))
        shot_observation="$ARTIFACTS/a11y-shot-keys-$shot_label-observation-$shot_attempt.json"
        "$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$shot_observation" 5 ||
            fail G-SCREENSHOT-KEYS "selection accessibility dump failed for $shot_label"
        cp "$shot_observation" "$ARTIFACTS/a11y-shot-keys-$shot_label-result.json"
        showing "$shot_observation" \
            "Selected area at x ${shot_position%,*}, y ${shot_position#*,}, width ${shot_dims%,*}, height ${shot_dims#*,}" && break
        [ "$(date +%s)" -lt "$end" ] ||
            fail G-SCREENSHOT-KEYS "$shot_label did not move/resize to the expected area"
        sleep 0.2
    done
    cstate ".active_workspace == $shot_workspace and (.overview_open | not)" "screenshot keeps workspace" ||
        fail G-SCREENSHOT-KEYS "$shot_label navigation escaped into desktop shortcuts"
    cstate ".pointer_position == [$shot_pointer]" "screenshot keyboard pointer feedback" ||
        fail G-SCREENSHOT-KEYS "$shot_label cursor did not follow the selected edge or area centre"
    cp "$CSTATE" "$ARTIFACTS/shot-keys-$shot_label-compositor.json"
    # Observe the visible nested host cursor too. Seat state alone can
    # change without moving Winit's actual pointer on the screen.
    "$PY" - "$WID" "$shot_pointer" "$WIDTH" "$HEIGHT" "$ARTIFACTS/shot-keys-$shot_label-host-cursor.json" <<'HOST_CURSOR' ||
import json, pathlib, subprocess, sys, time
window, expected, width, height, output = sys.argv[1:]
expected = [int(n) for n in expected.split(',')]
def fields(*args):
    return dict(line.split('=', 1) for line in subprocess.check_output(['xdotool', *args], text=True).splitlines() if '=' in line)
end = time.monotonic() + 5
while True:
    geometry = fields('getwindowgeometry', '--shell', window)
    cursor = fields('getmouselocation', '--shell')
    local = [int(cursor['X']) - int(geometry['X']), int(cursor['Y']) - int(geometry['Y'])]
    physical = [expected[0] * int(geometry['WIDTH']) / int(width), expected[1] * int(geometry['HEIGHT']) / int(height)]
    proof = {'expected_logical': expected, 'observed_window_local': local, 'expected_physical': physical, 'host_cursor': cursor, 'host_window': geometry}
    pathlib.Path(output).write_text(json.dumps(proof, indent=2) + '\n')
    if all(abs(a-b) <= 0.5 for a,b in zip(local, physical)):
        break
    if time.monotonic() >= end:
        raise SystemExit('visible host cursor did not follow screenshot selection')
    time.sleep(0.1)
HOST_CURSOR
        fail G-SCREENSHOT-KEYS "$shot_label visible host cursor did not follow keyboard selection"
    scrot "$ARTIFACTS/shot-keys-$shot_label-overlay.png" >/dev/null
    xdotool key --clearmodifiers Return
    end=$(($(date +%s) + 15))
    newest=""
    while [ "$(date +%s)" -le "$end" ]; do
        newest="$(find "$XDG_PICTURES_DIR/Screenshots" -name 'Screenshot From *.png' -newer "$marker" 2>/dev/null | head -1)"
        [ -n "$newest" ] && break
        sleep 0.3
    done
    [ -n "$newest" ] || fail G-SCREENSHOT-KEYS "$shot_label captured nothing"
    dims="$(ffprobe -v error -select_streams v:0 -show_entries stream=width,height -of csv=p=0 "$newest")"
    [ "$dims" = "$shot_dims" ] || fail G-SCREENSHOT-KEYS "$shot_label saved $dims, expected $shot_dims"
    cp "$newest" "$ARTIFACTS/shot-keys-$shot_label.png"
    cstate 'all(.layers[]; .namespace != "tuna-screenshot-ui")' "captured overlay unmapped" ||
        fail G-SCREENSHOT-KEYS "$shot_label did not close after capture"
done <<EOF
ctrl+Left ctrl+Right Left|$((WIDTH / 4 + 5)),$((HEIGHT / 4))|$((WIDTH * 3 / 8 - 5)),$((HEIGHT * 3 / 8))|resize|$((WIDTH * 3 / 8 - 5)),$((HEIGHT / 2 - 1))
Up ctrl+Up|$((WIDTH / 4)),$((HEIGHT / 4 + 1))|$((WIDTH * 3 / 8)),$((HEIGHT * 3 / 8 - 1))|vertical|$((WIDTH / 2 - 1)),$((HEIGHT * 3 / 8 - 1))
alt+ctrl+Right alt+shift+Down|$((WIDTH / 4)),$((HEIGHT / 4))|$((WIDTH * 3 / 8 + 1)),$((HEIGHT * 3 / 4))|move|$((WIDTH / 2)),$((HEIGHT * 7 / 8 - 1))
shift+Left|$((WIDTH * 5 / 8)),$((HEIGHT / 4))|0,$((HEIGHT * 3 / 8))|edge|0,$((HEIGHT / 2 - 1))
Down Down r ctrl+Up|$((WIDTH / 4)),$((HEIGHT / 4 - 1))|$((WIDTH * 3 / 8)),$((HEIGHT * 3 / 8))|reset-edge|$((WIDTH / 2 - 1)),$((HEIGHT * 5 / 8 - 2))
ctrl+Left r|$((WIDTH / 4)),$((HEIGHT / 4))|$((WIDTH * 3 / 8)),$((HEIGHT * 3 / 8))|reset|$((WIDTH * 3 / 8 - 1)),$((HEIGHT / 2 - 1))
EOF
pass G-SCREENSHOT-KEYS "arrows, Ctrl, Alt, Shift and R produce actual captures with bounded selection dimensions and GNOME cursor feedback"

# Modified navigation in Screen mode must neither edit the hidden area nor
# leak into desktop workspace switching or overview shortcuts.
pointer_before=$(jq -c '.pointer_position' "$CSTATE")
workspace_before=$(jq -r '.active_workspace' "$CSTATE")
xdotool key Print
cstate 'any(.layers[]; .namespace == "tuna-screenshot-ui")' "screen-mode overlay mapped" ||
    fail G-SCREENSHOT-KEYS "Print did not map Screen-mode control"
end=$(($(date +%s) + 20))
until "$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/a11y-shot-screen-control.json" 5 >/dev/null 2>&1 &&
    showing "$ARTIFACTS/a11y-shot-screen-control.json" "Capture" || [ "$(date +%s)" -gt "$end" ]; do sleep 0.5; done
showing "$ARTIFACTS/a11y-shot-screen-control.json" "Capture" ||
    fail G-SCREENSHOT-KEYS "Print did not open screen-mode negative control"
xdotool key --clearmodifiers c ctrl+Left alt+shift+Down Escape
cstate 'all(.layers[]; .namespace != "tuna-screenshot-ui")' "screen-mode overlay unmapped" ||
    fail G-SCREENSHOT-KEYS "Escape did not unmap Screen-mode control"
end=$(($(date +%s) + 10))
while :; do
    "$PY" "$ROOT/scripts/lib/tuna-a11y-dump.py" tuna-shell-gtk "$ARTIFACTS/a11y-shot-screen-dismissed.json" 5 ||
        fail G-SCREENSHOT-KEYS "dismissal accessibility dump failed"
    showing "$ARTIFACTS/a11y-shot-screen-dismissed.json" "Capture" || break
    [ "$(date +%s)" -lt "$end" ] || fail G-SCREENSHOT-KEYS "Escape did not dismiss Screen-mode control"
    sleep 0.2
done
sleep 0.3 # Let the compositor publish the processed keyboard sequence.
cstate ".pointer_position == $pointer_before and .active_workspace == $workspace_before and (.overview_open | not)" "screen-mode navigation negative control" ||
    fail G-SCREENSHOT-KEYS "modified Screen-mode keys moved the pointer or leaked desktop shortcuts"
cp "$CSTATE" "$ARTIFACTS/shot-screen-negative-control.json"
pass G-SCREENSHOT-KEYS-SCREEN "modified Screen navigation leaves pointer and workspace unchanged; Escape dismisses"

}
