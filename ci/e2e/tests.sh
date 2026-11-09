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
xrandr --query > "$LOGS/xrandr.log" 2>&1 # documents Xvfb's 0 Hz mode

# X11 tools can block forever if a client wedges the server; bound them.
xdotool() { timeout 20 command xdotool "$@"; }
xprop() { timeout 20 command xprop "$@"; }
import() { timeout 30 command import "$@"; }
convert() { timeout 30 command convert "$@"; }
gio() { timeout 60 command gio "$@"; }
now() { date +%s.%N; }
since() { awk "BEGIN{printf \"%.1f\", $(now) - $1}"; }
record() { # app check PASS|FAIL|KNOWN seconds detail (KNOWN: upstream issue, documented)
    printf '%s\t%s\t%s\t%s\t%s\n' "$1" "$2" "$3" "$4" "${5:-}" >> "$RESULTS"
    echo "[$3] $1: $2 (${4}s) ${5:-}"
    [ "$3" = PASS ] || [ "$3" = KNOWN ] || FAILED=1
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
stop_prefix() { WINEPREFIX=$(prefix_of "$1") timeout 30 wineserver -k 2>/dev/null; WINEPREFIX=$(prefix_of "$1") timeout 30 wineserver -w 2>/dev/null; }

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
    local t0 out pj=$LOGS/hello-first-progress.jsonl
    t0=$(now)
    # Machine-readable progress for a launch sheet, on fd 3.
    if out=$(lbrun hello-first 300 --progress-fd 3 "$PROGS/hello.exe" 3>"$pj") && grep -q 'hello from Windows' "$LOGS/hello-first.log"; then
        record hello "first run (new prefix + program)" PASS "$(since "$t0")" "$(grep -o 'USERNAME=.*' "$LOGS/hello-first.log" | head -1)"
    else
        record hello "first run (new prefix + program)" FAIL "$(since "$t0")" "see logs/hello-first.log"
        snap hello-first-failure; windows >> "$LOGS/hello-first.log"
    fi
    if jq -e -s 'all(.v == 1) and .[0].event == "start" and .[0].first_launch == true
                 and (map(select(.event == "step") | .step)
                      | (index("prepare") != null) and (index("prepare") < index("launch")))
                 and (map(.event) | index("launched")) != null
                 and .[-1] == {"v": 1, "event": "exit", "code": 0}' "$pj" >/dev/null 2>&1; then
        record hello "progress as JSON lines (--progress-fd)" PASS 0 "$(jq -r -s 'map(.event) | [.[0]] + (.[1:] | unique) | join(" ")' "$pj") ($(wc -l < "$pj") lines)"
    else
        record hello "progress as JSON lines (--progress-fd)" FAIL 0 "see logs/hello-first-progress.jsonl"
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
        # Started like on Windows for an administrator with UAC: not elevated
        # (Notepad++ says "[Administrator]" in its title when it is).
        if [[ "$title" == *"[Administrator]"* ]]; then
            record "$name" "not running as Administrator" FAIL 0 "title: $title"
        else
            record "$name" "not running as Administrator" PASS 0 "$(pgrep -a -f 'lucent-start\.exe' | grep -c . | sed 's/$/ lucent-start.exe process(es)/')"
        fi
        snap "$name"
    else
        snap "$name-launch-failure"; windows >> "$LOGS/$name-launch.log"
        record "$name" "window from launcher (WM_CLASS=$wmclass)" FAIL "$(since "$t0")" "no window; see logs/$name-launch.log and $name-debug.log"
        # Same program again with Wine's diagnostics on, for the log.
        local target; target=$(sed -n 's/^Exec=.* run "\{0,1\}\([^"]*\.exe\)"\{0,1\}.*/\1/p' "$entry")
        WINEDEBUG=err+all,warn+module,+seh,+mscoree timeout 90 lucent-bridge run "$target" > "$LOGS/$name-debug.full.log" 2>&1
        { head -c 200000 "$LOGS/$name-debug.full.log"; } > "$LOGS/$name-debug.log"; rm -f "$LOGS/$name-debug.full.log"
        stop_prefix "$id"
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
d3d11_attempt() { # label -> records the result; returns 0 if it rendered
    local label=$1 exe=$PROGS/d3d11_sample.exe t0 id wid pid
    id=$(lucent-bridge inspect --json "$exe" | jq -r .app_id)
    mkdir -p "$LOGS/dxvk"
    t0=$(now)
    DXVK_LOG_PATH=$LOGS/dxvk DXVK_LOG_LEVEL=info lbrun "d3d11-$label" 300 "$exe" &
    pid=$!
    local ok=1
    if wid=$(wait_window name '^LB D3D11 sample - ' 240 "$pid"); then
        sleep 5
        import -window "$wid" "$SHOTS/d3d11.png" 2>/dev/null
        local title mid corner
        title=$(xdotool getwindowname "$wid" 2>/dev/null)
        mid=$(convert "$SHOTS/d3d11.png" -format '%[fx:int(255*p{320,300}.r)],%[fx:int(255*p{320,300}.g)],%[fx:int(255*p{320,300}.b)]' info: 2>/dev/null)
        corner=$(convert "$SHOTS/d3d11.png" -format '%[fx:int(255*p{20,20}.r)],%[fx:int(255*p{20,20}.g)],%[fx:int(255*p{20,20}.b)]' info: 2>/dev/null)
        local backend=wined3d; compgen -G "$LOGS/dxvk/*.log" >/dev/null && backend=DXVK
        local dxvk_version; dxvk_version=$(grep -h -o -m1 'DXVK: v[0-9.]*' "$LOGS"/dxvk/*.log 2>/dev/null | head -1)
        IFS=, read -r r g b <<< "${mid:-0,0,0}"
        if [ -z "$mid" ]; then
            # The window went away: let the process finish writing why.
            for _ in $(seq 30); do kill -0 "$pid" 2>/dev/null || break; sleep 1; done
        fi
        if [ -n "$mid" ] && ((g > 150 && r < 80 && b < 110)) && [ "$backend" = DXVK ]; then
            record d3d11 "renders a triangle ($backend, $label)" PASS "$(since "$t0")" "${dxvk_version:-DXVK}; $title; centre rgb($mid) corner rgb($corner)"
            ok=0
        elif [ -n "$mid" ] && ((g > 150 && r < 80 && b < 110)); then
            # Lucent Bridge ships DXVK on x86-64 and AArch64's Wine has it:
            # wined3d means DXVK wasn't used.
            record d3d11 "renders a triangle ($backend, $label)" FAIL "$(since "$t0")" "rendered, but with wined3d instead of DXVK; $title"
        elif grep -q 'Unhandled division by zero' "$LOGS/d3d11-$label.log" && grep -q 'DXVK: 2\.7' "$LOGS/d3d11-$label.log"; then
            record d3d11 "renders a triangle ($backend, $label)" KNOWN "$(since "$t0")" "DXVK 2.7.1 divides by the display refresh rate, and Xvfb reports 0 Hz (guarded upstream after 2.7.1)"
        else
            record d3d11 "renders a triangle ($backend, $label)" FAIL "$(since "$t0")" "$title; centre rgb($mid) corner rgb($corner)"
            stop_prefix "$id"
            WINEDEBUG=err+all,+seh,+loaddll DXVK_LOG_LEVEL=debug timeout 60 lucent-bridge run "$exe" > "$LOGS/d3d11-$label-debug.full.log" 2>&1
            grep -v 'trace:seh:libc_addr_cb' "$LOGS/d3d11-$label-debug.full.log" | tail -c 300000 > "$LOGS/d3d11-$label-debug.log"
            rm -f "$LOGS/d3d11-$label-debug.full.log"
        fi
    else
        snap "d3d11-$label-failure"; windows >> "$LOGS/d3d11-$label.log"
        record d3d11 "renders a triangle ($label)" FAIL "$(since "$t0")" "no window; see logs/d3d11-$label.log"
    fi
    local sys32; sys32=$(prefix_of "$id")/drive_c/windows/system32
    { echo "d3d11.dll in the prefix:"; grep -a -o -i -m3 'dxvk [a-z0-9.]*\|Wine builtin DLL' "$sys32/d3d11.dll" | sort -u; } >> "$LOGS/d3d11-$label.log" 2>&1
    stop_prefix "$id"
    wait "$pid" 2>/dev/null
    return $ok
}

# Direct3D 11 with DXVK on the software Vulkan driver. Xvfb's only mode has
# no refresh rate (0 Hz; logs/xrandr.log). DXVK 2.7.1, which Fedora 45 ships
# for x86_64, computes 1e9*den/num from it unguarded and dies with an
# integer division by zero; Lucent Bridge therefore bundles DXVK 3.x on
# x86-64 (guarded), and AArch64's Wine comes with DXVK 3.x. (The KNOWN
# branch above only remains for running this against an older build.)
d3d11() {
    d3d11_attempt xvfb-0hz
}

# ---------------------------------------------------------------------------
# 4. Rights: programs start like on Windows for an administrator with UAC.
# ---------------------------------------------------------------------------
admin_check() { # label exe expected-title-regex
    local label=$1 exe=$2 want=$3 t0 wid pid title id slug
    id=$(lucent-bridge inspect --json "$exe" | jq -r .app_id)
    slug=$(printf '%s' "${label,,}" | tr -c 'a-z0-9' '-')   # a file name artifacts accept
    t0=$(now)
    lbrun "admin-$slug" 180 "$exe" &
    pid=$!
    if wid=$(wait_window name '^LB admin check: ' 150 "$pid"); then
        title=$(xdotool getwindowname "$wid" 2>/dev/null)
        if [[ "$title" =~ $want ]]; then
            record admin "$label" PASS "$(since "$t0")" "$title"
        else
            record admin "$label" FAIL "$(since "$t0")" "'$title' !~ /$want/"
        fi
    else
        record admin "$label" FAIL "$(since "$t0")" "no window; see logs/admin-$slug.log"
        windows >> "$LOGS/admin-$slug.log"
    fi
    stop_prefix "$id"
    wait "$pid" 2>/dev/null
}
rights() {
    # An ordinary program: the filtered token, like Notepad++ from its entry.
    admin_check "program: not Administrator" "$PROGS/admincheck.exe" 'admins=0 elevated=0'
    # Its manifest asks for administrator rights: Wine elevates it, as UAC would.
    admin_check "manifest requireAdministrator: elevated" "$PROGS/admincheck-admin.exe" 'admins=1 elevated=1'
    # An installer keeps the rights it always had (Program Files installs).
    cp "$PROGS/admincheck.exe" "$HOME/admincheck-setup.exe"
    admin_check "installer: Administrator" "$HOME/admincheck-setup.exe" 'admins=1'
}

# ---------------------------------------------------------------------------
# 5. Looks: dark mode at launch, and DPI from the desktop's scale.
# ---------------------------------------------------------------------------
relaunch() { # name entry-name-regex shot -> screenshot of the app started from its entry
    local name=$1 entry_re=$2 shot=$3 entry="" f id wmclass wid class_re
    for f in "$HOME"/.local/share/applications/lucent-bridge-*.desktop; do
        grep -Eq "^Name=($entry_re)\$" "$f" && entry=$f
    done
    if [ -z "$entry" ]; then
        record "$name" "$shot" FAIL 0 "no launcher entry"
        return
    fi
    id=$(sed -n 's/^Exec=.*\/prefixes\/\([^/]*\)\/.*/\1/p' "$entry")
    wmclass=$(sed -n 's/^StartupWMClass=//p' "$entry")
    class_re=$(printf '%s' "$wmclass" | sed 's/[][\.*^$+?(){}|]/\\&/g')
    gio launch "$entry" >> "$LOGS/$name-$shot.log" 2>&1 &
    if wid=$(wait_window class "^${class_re}\$" 180); then
        sleep 4
        snap "$name-$shot"
        record "$name" "$shot: window" PASS 0 "$(xdotool getwindowname "$wid" 2>/dev/null)"
    else
        snap "$name-$shot-failure"
        record "$name" "$shot: window" FAIL 0 "no window"
    fi
    close_app "$name" "$id" "$(basename "$(sed -n 's/^Exec=.* run //p' "$entry" | tr -d '"' | sed 's/ --.*//')")"
}
reg_value() { # prefix key value -> raw text after "value"= in user.reg
    # Key names are written with doubled backslashes there. ENVIRON, not
    # awk -v, so awk doesn't process the backslashes again.
    K="[$(printf '%s' "$2" | sed 's/\\/\\\\/g')]" V="\"$3\"=" awk '
        index($0, ENVIRON["K"]) == 1 { s = 1; next }
        /^$/ { s = 0 }
        s && index($0, ENVIRON["V"]) == 1 { print substr($0, length(ENVIRON["V"]) + 1); exit }' "$1/user.reg"
}
looks() {
    local npp; npp=$(ls -d "$HOME"/.local/share/lucent-bridge/prefixes/notepadplusplus-* 2>/dev/null | head -n1)
    # What Plasma writes to ~/.config/kdeglobals when the user picks the
    # dark look (Lucent Dark's window background).
    mkdir -p "$HOME/.config"
    printf '[General]\nColorScheme=LucentDark\n\n[Colors:Window]\nBackgroundNormal=29,31,46\n' > "$HOME/.config/kdeglobals"
    relaunch notepadplusplus 'Notepad\+\+' dark
    relaunch 7-zip '7-Zip File Manager' dark
    relaunch keepass 'KeePass( 2)?' dark
    local light win; light=$(reg_value "$npp" 'Software\Microsoft\Windows\CurrentVersion\Themes\Personalize' AppsUseLightTheme)
    win=$(reg_value "$npp" 'Control Panel\Colors' Window)
    if [ "$light" = dword:00000000 ] && [ "$win" = '"44 47 68"' ]; then
        record looks "dark mode followed at launch" PASS 0 "AppsUseLightTheme=$light, Window=$win"
    else
        record looks "dark mode followed at launch" FAIL 0 "AppsUseLightTheme=${light:-unset}, Window=${win:-unset}"
    fi
    rm -f "$HOME/.config/kdeglobals"
    # Plasma at 150 %: KWin gives X11 programs Xft.dpi 144.
    echo 'Xft.dpi: 144' | xrdb -nocpp -merge
    local xft; xft=$(xrdb -query 2>&1 | grep -i 'dpi' | tr '\t\n' '  ')
    relaunch notepadplusplus 'Notepad\+\+' 144dpi
    xrdb -remove
    local dpi; dpi=$(reg_value "$npp" 'Control Panel\Desktop' LogPixels)
    local said; said=$(grep -h -o 'appearance[^|]*' "$LOGS/notepadplusplus-144dpi.log" 2>/dev/null | tail -n1)
    if [ "$dpi" = dword:00000090 ]; then
        record looks "DPI follows the desktop scale (Xft.dpi 144)" PASS 0 "LogPixels=$dpi; xrdb: $xft; lucent-bridge: $said"
    else
        record looks "DPI follows the desktop scale (Xft.dpi 144)" FAIL 0 "LogPixels=${dpi:-unset}; xrdb: ${xft:-nothing}; lucent-bridge: ${said:-said nothing}"
    fi
    local font; font=$(grep -a -o -m1 '"MS Shell Dlg"="[^"]*"' "$npp/system.reg")
    if [ "$font" = '"MS Shell Dlg"="Inter"' ] && grep -q '^"Inter"=str(7):"tahoma.ttf,Tahoma' "$npp/system.reg"; then
        record looks "dialogs use the desktop's UI font, Tahoma as fallback" PASS 0 "$font"
    else
        record looks "dialogs use the desktop's UI font, Tahoma as fallback" FAIL 0 "${font:-no MS Shell Dlg substitute}"
    fi
    # Back to light for whatever comes next.
    relaunch notepadplusplus 'Notepad\+\+' light-again
}

first_run
install_and_launch notepadplusplus "$APPS/npp.8.9.9.Installer.x64.exe" 'Notepad\+\+' 'Notepad\+\+' /S
install_and_launch 7-zip "$APPS/7z2606-x64.exe" '7-Zip File Manager' '.' /S
install_and_launch keepass "$APPS/KeePass-2.62-Setup.exe" 'KeePass( 2)?' 'KeePass' /VERYSILENT /SUPPRESSMSGBOXES /NORESTART /SP-
d3d11
rights
if [ "${LUCENT_BRIDGE_APPEARANCE:-auto}" != off ]; then
    looks
fi

lucent-bridge list > "$LOGS/list.txt" 2>&1
{
    echo "### lucent-bridge e2e on $LB_ARCH (template: $LB_TEMPLATE)"
    echo
    echo "| app | check | result | s | detail |"
    echo "|---|---|---|---|---|"
    sed 's/|/\\|/g; s/\t/ | /g; s/^/| /; s/$/ |/' "$RESULTS"
} > "$OUT/summary.md"
exit $FAILED
