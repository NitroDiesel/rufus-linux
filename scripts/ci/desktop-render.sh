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
    # The outer five-pixel gutter is canvas from top to bottom at every size.
    # A stale 600px GLX drawable leaves black pixels below the rendered content.
    local colors corner
    colors=$("$image_command" "$shot" -crop 1x0+5+0 +repage -format '%k' info:)
    corner=$("$image_command" "$shot" -format '%[hex:p{5,5}]' info:)
    if [[ "$colors" != 1 || "$corner" != "$canvas" ]]; then
        echo "Incomplete desktop frame: $shot (colors=$colors, canvas=$corner)" >&2
        return 1
    fi
}

for theme in light dark; do
    canvas=F2F5F6
    [[ "$theme" != dark ]] || canvas=10171E
    for scale in 1 1.25 2; do
        env -u WAYLAND_DISPLAY SLINT_BACKEND=winit-gl SLINT_SCALE_FACTOR="$scale" \
            WINIT_X11_SCALE_FACTOR=1 LIBGL_ALWAYS_SOFTWARE=1 RUFUS_LINUX_THEME="$theme" \
            "$binary" &
        app_pid=$!
        window=$(timeout 15s xdotool search --sync --onlyvisible --pid "$app_pid" --name '^Rufus Linux$' | head -n 1)
        sleep 2
        case "$scale" in
            1) startup_size=560x720; minimum_width=480; minimum_height=560 ;;
            1.25) startup_size=700x900; minimum_width=600; minimum_height=700 ;;
            2) startup_size=1120x1440; minimum_width=960; minimum_height=1120 ;;
        esac
        check_frame "$theme-$scale-startup" "$startup_size"
        xdotool windowsize "$window" "$minimum_width" "$minimum_height"
        sleep 1
        check_frame "$theme-$scale-minimum" "${minimum_width}x${minimum_height}"
        xdotool windowsize "$window" 2200 1800
        sleep 1
        check_frame "$theme-$scale-large" 2200x1800
        cleanup
        app_pid=
    done
done
