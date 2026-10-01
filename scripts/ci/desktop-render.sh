#!/usr/bin/env bash
# Run inside a dedicated Xvfb display without a window manager.
set -euo pipefail

binary=$(realpath "${1:?desktop binary required}")
artifacts=${2:?screenshot directory required}
mkdir -p "$artifacts"
image_command=convert
if command -v magick >/dev/null; then
    image_command=magick
fi
app_pid=
cleanup() {
    if [[ -n "$app_pid" ]]; then
        kill -TERM "$app_pid" 2>/dev/null || true
        wait "$app_pid" 2>/dev/null || true
    fi
}
trap cleanup EXIT

check_frame() {
    local name=$1
    local expected_size=$2
    local shot="$artifacts/$name.png"
    import -window "$window" "$shot"
    local actual_size
    actual_size=$("$image_command" "$shot" -format '%wx%h' info:)
    if [[ "$actual_size" != "$expected_size" ]]; then
        echo "Wrong desktop size: $shot ($actual_size, expected $expected_size)" >&2
        return 1
    fi
    # The five-pixel gutter is canvas at the top, middle, and bottom of the
    # window; only the full-width header and footer hairlines cross it.
    # A stale 600px GLX drawable leaves black pixels below the rendered content.
    local height point color
    height=${actual_size#*x}
    for point in "5,5" "5,$((height / 2))" "5,$((height - 5))"; do
        color=$("$image_command" "$shot" -format "%[hex:p{$point}]" info:)
        if [[ "$color" != "$canvas" ]]; then
            echo "Incomplete desktop frame: $shot (pixel $point=$color, canvas=$canvas)" >&2
            return 1
        fi
    done
}

for theme in light dark; do
    canvas=FCFCFC
    [[ "$theme" != dark ]] || canvas=0A0A0A
    for scale in 1 1.25 2; do
        env -u WAYLAND_DISPLAY SLINT_BACKEND=winit-gl SLINT_SCALE_FACTOR="$scale" \
            WINIT_X11_SCALE_FACTOR=1 LIBGL_ALWAYS_SOFTWARE=1 RUFUS_LINUX_THEME="$theme" \
            "$binary" &
        app_pid=$!
        window=$(timeout 15s xdotool search --sync --onlyvisible --pid "$app_pid" --name '^Rufus Linux$' | head -n 1)
        sleep 2
        case "$scale" in
            1) startup_size=560x720 ;;
            1.25) startup_size=700x900 ;;
            2) startup_size=1120x1440 ;;
        esac
        check_frame "$theme-$scale-startup" "$startup_size"
        # Like upstream Rufus the dialog has a fixed size: the window manager
        # hints pin minimum and maximum to the startup size.
        hints=$(xprop -id "$window" WM_NORMAL_HINTS)
        for bound in minimum maximum; do
            if ! grep -q "program specified $bound size: ${startup_size/x/ by }" <<<"$hints"; then
                echo "Window is not fixed at $startup_size ($bound):" >&2
                echo "$hints" >&2
                exit 1
            fi
        done
        cleanup
        app_pid=
    done
done
