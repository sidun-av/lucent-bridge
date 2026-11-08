#!/usr/bin/env bash
# The end-to-end tests, run as an ordinary user with no network.
#   $1 installers   $2 test programs built from source   $3 output dir
# Every check writes one line to $OUT/results.tsv:
#   app <TAB> check <TAB> PASS|FAIL <TAB> seconds <TAB> detail
# The script keeps going after a failure (so one run shows everything) and
# exits non-zero at the end if anything failed.
set -uo pipefail
APPS=$1 PROGS=$2 OUT=$3
SHOTS=$OUT/screenshots LOGS=$OUT/logs
mkdir -p "$SHOTS" "$LOGS"
RESULTS=$OUT/results.tsv
: > "$RESULTS"
FAILED=0

export DISPLAY=:99 XDG_RUNTIME_DIR=/tmp/xdg-$UID LANG=C.UTF-8
mkdir -p -m 700 "$XDG_RUNTIME_DIR"
Xvfb :99 -screen 0 1280x800x24 -nolisten tcp >"$LOGS/xvfb.log" 2>&1 &
XVFB=$!
trap 'kill $XVFB 2>/dev/null' EXIT
for _ in $(seq 50); do xwininfo -root >/dev/null 2>&1 && break; sleep 0.2; done

now() { date +%s.%N; }
since() { awk "BEGIN{printf \"%.1f\", $(now) - $1}"; }
record() { # app check PASS|FAIL seconds detail
    printf '%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" "${5:-}" >> "$RESULTS"
    echo "[$3] $1: $2 (${4}s) ${5:-}"
    [ "$3" = PASS ] || FAILED=1
}
windows() { # every mapped window with its title, for failure diagnostics
    xdotool search --onlyvisible --name '.*' 2>/dev/null | while read -r w; do
        echo "  $w $(xdotool getwindowname "$w" 2>/dev/null) [$(xprop -id "$w" WM_CLASS 2>/dev/null | cut -d= -f2)]"
    done
}
snap() { import -window root "$SHOTS/$1.png" 2>/dev/null; }
wait_window() { # by(name|class) pattern timeout [pid] -> window id
    local deadline=$((SECONDS + $3)) ids
    while ((SECONDS < deadline)); do
        ids=$(xdotool search --onlyvisible "--$1" "$2" 2>/dev/null) && { echo "$ids" | tail -1; return 0; }
        # Stop early once the launching process is gone and no window came.
        if [ -n "${4:-}" ] && ! kill -0 "$4" 2>/dev/null; then
            sleep 2
            ids=$(xdotool search --onlyvisible "--$1" "$2" 2>/dev/null) && { echo "$ids" | tail -1; return 0; }
            return 1
        fi
        sleep 1
    done
    return 1
}
prefix_of() { echo "$HOME/.local/share/lucent-bridge/prefixes/$1"; }
stop_prefix() { WINEPREFIX=$(prefix_of "$1") timeout 30 wineserver -k 2>/dev/null; WINEPREFIX=$(prefix_of "$1") wineserver -w 2>/dev/null; }

lbrun() { # log-name timeout args...
    local log=$LOGS/$1.log t=$2; shift 2
    echo "\$ lucent-bridge run $*" >> "$log"
    timeout "$t" lucent-bridge run "$@" >>"$log" 2>&1
}

silent_flags() { # installer -> args for `lucent-bridge run`
    if lucent-bridge run --help | grep -q -- '--silent'; then
        echo --silent
    fi
}

# ---------------------------------------------------------------------------
# 1. First-run cost: a console program in a brand-new prefix, then again.
# ---------------------------------------------------------------------------
first_run() {
    local t0 out
    t0=$(now)
    if out=$(lbrun hello-first 300 "$PROGS/hello.exe") && grep -q 'hello from Windows' "$LOGS/hello-first.log"; then
        record hello "first run (new prefix + program)" PASS "$(since "$t0")" "$(grep -o 'USERNAME=.*' "$LOGS/hello-first.log" | head -1)"
    else
        record hello "first run (new prefix + program)" FAIL "$(since "$t0")" "see logs/hello-first.log"
        snap hello-first-failure; windows >> "$LOGS/hello-first.log"
    fi
    local id; id=$(lucent-bridge inspect --json "$PROGS/hello.exe" | jq -r .app_id)
    stop_prefix "$id"
    t0=$(now)
    if lbrun hello-second 120 "$PROGS/hello.exe" && grep -q 'hello from Windows' "$LOGS/hello-second.log"; then
        record hello "second run (prefix exists)" PASS "$(since "$t0")"
    else
        record hello "second run (prefix exists)" FAIL "$(since "$t0")" "see logs/hello-second.log"
    fi
    stop_prefix "$id"
    # Did Wine install Mono and Gecko from the local packages (no dialog, no download)?
    local p; p=$(prefix_of "$id")
    if [ -d "$p/drive_c/windows/mono" ] || grep -qi 'Wine Mono' "$p/system.reg" 2>/dev/null; then
        record hello "Wine Mono installed offline" PASS 0
    else
        record hello "Wine Mono installed offline" FAIL 0 "no mono in prefix"
    fi
    if grep -qi 'Wine Gecko' "$p/system.reg" 2>/dev/null || compgen -G "$p/drive_c/windows/*/gecko" >/dev/null; then
        record hello "Wine Gecko installed offline" PASS 0
    else
        record hello "Wine Gecko installed offline" FAIL 0 "no gecko in prefix"
    fi
    du -sh "$p" > "$LOGS/prefix-size.txt" 2>&1
    local ready; ready=$(grep -o 'environment ready in [0-9.]* s ([a-z ]*)' "$LOGS/hello-first.log" | head -1)
    record hello "prefix creation: ${ready:-not reported}" PASS 0
    if grep -q 'USERPROFILE=C:\\users\\tester' "$LOGS/hello-first.log"; then
        record hello "profile belongs to the real user" PASS 0
    else
        record hello "profile belongs to the real user" FAIL 0 "$(grep -o 'USERPROFILE=.*' "$LOGS/hello-first.log")"
    fi
    if [ "$LB_TEMPLATE" = 1 ]; then
        local left; left=$(grep -ril --exclude-dir=windows lucent-template "$p" 2>/dev/null | head -5)
        if [ -z "$left" ] && [ ! -e "$p/drive_c/users/lucent-template" ]; then
            record hello "no template user left in the prefix" PASS 0
        else
            record hello "no template user left in the prefix" FAIL 0 "$left"
        fi
    fi
}

# ---------------------------------------------------------------------------
# 2. Install a real program, check its launcher entry, start it from there.
# ---------------------------------------------------------------------------
install_and_launch() { # name installer entry-name-regex title-regex [installer args...]
    local name=$1 installer=$2 entry_re=$3 title_re=$4; shift 4
    local t0 id flags
    id=$(lucent-bridge inspect --json "$installer" | jq -r .app_id)
    flags=$(silent_flags)
    lucent-bridge plan "$installer" > "$LOGS/$name-plan.txt" 2>&1
    t0=$(now)
    if [ -n "$flags" ]; then
        lbrun "$name-install" 600 $flags "$installer"
    else
        lbrun "$name-install" 600 "$installer" -- "$@"
    fi
    local rc=$?
    if [ $rc -ne 0 ]; then
        snap "$name-install-failure"; windows >> "$LOGS/$name-install.log"
        record "$name" "silent install" FAIL "$(since "$t0")" "exit $rc, see logs/$name-install.log"
        stop_prefix "$id"; return
    fi
    record "$name" "silent install" PASS "$(since "$t0")"
    stop_prefix "$id"

    # Launcher entry: ours, valid, with an existing PNG icon.
    local apps=$HOME/.local/share/applications entry=""
    for f in "$apps"/lucent-bridge-"$id"-*.desktop; do
        [ -f "$f" ] || continue
        cp "$f" "$LOGS/"
        if grep -Eq "^Name=($entry_re)\$" "$f"; then entry=$f; fi
    done
    if [ -z "$entry" ]; then
        record "$name" "launcher entry" FAIL 0 "none named /$entry_re/ in: $(ls "$apps" 2>/dev/null | tr '\n' ' ')"
        find "$(prefix_of "$id")/drive_c" -iname '*.lnk' >> "$LOGS/$name-install.log" 2>&1
        return
    fi
    if desktop-file-validate "$entry" > "$LOGS/$name-validate.txt" 2>&1; then
        record "$name" "launcher entry valid ($(basename "$entry"))" PASS 0
    else
        record "$name" "launcher entry valid" FAIL 0 "$(tr '\n' ' ' < "$LOGS/$name-validate.txt")"
    fi
    local icon; icon=$(sed -n 's/^Icon=//p' "$entry")
    if [ -f "$icon" ] && file "$icon" | grep -q 'PNG image'; then
        cp "$icon" "$SHOTS/$name-icon.png"
        record "$name" "icon" PASS 0 "$(identify -format '%wx%h' "$icon")"
    else
        record "$name" "icon" FAIL 0 "Icon=$icon"
    fi

    # Start it the way the desktop does, and look for its window.
    local wmclass; wmclass=$(sed -n 's/^StartupWMClass=//p' "$entry")
    t0=$(now)
    gio launch "$entry" >> "$LOGS/$name-launch.log" 2>&1 &
    local wid
    local class_re; class_re=$(printf '%s' "$wmclass" | sed 's/[][\.*^$+?(){}|]/\\&/g')
    if wid=$(wait_window class "^${class_re}\$" 180); then
        sleep 3 # let it finish painting
        local title; title=$(xdotool getwindowname "$wid")
        if [[ "$title" =~ $title_re ]]; then
            record "$name" "window from launcher (WM_CLASS=$wmclass)" PASS "$(since "$t0")" "title: $title"
        else
            record "$name" "window from launcher (WM_CLASS=$wmclass)" FAIL "$(since "$t0")" "title '$title' !~ /$title_re/"
        fi
        snap "$name"
    else
        snap "$name-launch-failure"; windows >> "$LOGS/$name-launch.log"
        record "$name" "window from launcher (WM_CLASS=$wmclass)" FAIL "$(since "$t0")" "no window; see logs/$name-launch.log"
    fi
    close_app "$name" "$id" "$(basename "$(sed -n 's/^Exec=.* run //p' "$entry" | tr -d '"' | sed 's/ --.*//')")"
}

close_app() { # name app-id exe-name: WM_CLOSE via taskkill, like the user clicking X
    local p; p=$(prefix_of "$2")
    WINEPREFIX=$p timeout 30 wine taskkill /IM "$3" >> "$LOGS/$1-launch.log" 2>&1
    if WINEPREFIX=$p timeout 30 wineserver -w; then
        record "$1" "closes cleanly" PASS 0
    else
        record "$1" "closes cleanly" FAIL 0 "still running after WM_CLOSE"
        stop_prefix "$2"
    fi
}

# ---------------------------------------------------------------------------
# 3. Direct3D 11 (DXVK where installed) on the software Vulkan/GL driver.
# ---------------------------------------------------------------------------
d3d11() {
    local exe=$PROGS/d3d11_sample.exe t0 id wid
    id=$(lucent-bridge inspect --json "$exe" | jq -r .app_id)
    mkdir -p "$LOGS/dxvk"
    t0=$(now)
    DXVK_LOG_PATH=$LOGS/dxvk DXVK_LOG_LEVEL=info lbrun d3d11 300 "$exe" &
    local pid=$!
    if wid=$(wait_window name '^LB D3D11 sample - ' 240 "$pid"); then
        sleep 5
        import -window "$wid" "$SHOTS/d3d11.png" 2>/dev/null; snap d3d11-screen
        local title; title=$(xdotool getwindowname "$wid")
        local mid corner
        mid=$(convert "$SHOTS/d3d11.png" -format '%[fx:int(255*p{320,300}.r)],%[fx:int(255*p{320,300}.g)],%[fx:int(255*p{320,300}.b)]' info:)
        corner=$(convert "$SHOTS/d3d11.png" -format '%[fx:int(255*p{20,20}.r)],%[fx:int(255*p{20,20}.g)],%[fx:int(255*p{20,20}.b)]' info:)
        local backend=wined3d; compgen -G "$LOGS/dxvk/*.log" >/dev/null && backend=DXVK
        local sys32; sys32=$(prefix_of "$id")/drive_c/windows/system32
        { echo "d3d11.dll in the prefix:"; grep -a -o -i -m3 'dxvk [a-z0-9.]*\|Wine builtin DLL' "$sys32/d3d11.dll" | sort -u; } >> "$LOGS/d3d11.log" 2>&1
        IFS=, read -r r g b <<< "$mid"
        if ((g > 150 && r < 80 && b < 110)); then
            record d3d11 "renders a triangle ($backend)" PASS "$(since "$t0")" "$title; centre rgb($mid) corner rgb($corner)"
        else
            record d3d11 "renders a triangle ($backend)" FAIL "$(since "$t0")" "$title; centre rgb($mid) corner rgb($corner)"
        fi
    else
        snap d3d11-failure; windows >> "$LOGS/d3d11.log"
        record d3d11 "renders a triangle" FAIL "$(since "$t0")" "no window; see logs/d3d11.log"
    fi
    stop_prefix "$id"
    wait "$pid" 2>/dev/null
}

first_run
install_and_launch notepadplusplus "$APPS/npp.8.9.9.Installer.x64.exe" 'Notepad\+\+' 'Notepad\+\+' /S
install_and_launch 7-zip "$APPS/7z2606-x64.exe" '7-Zip File Manager' '.' /S
install_and_launch keepass "$APPS/KeePass-2.62-Setup.exe" 'KeePass( 2)?' 'KeePass' /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP-
d3d11

lucent-bridge list > "$LOGS/list.txt" 2>&1
{
    echo "### lucent-bridge e2e on $LB_ARCH (template: $LB_TEMPLATE)"
    echo
    echo "| app | check | result | s | detail |"
    echo "|---|---|---|---|---|"
    sed 's/|/\\|/g; s/\t/ | /g; s/^/| /; s/$/ |/' "$RESULTS"
} > "$OUT/summary.md"
exit $FAILED
