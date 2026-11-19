#!/usr/bin/env bash
# 在目标机器上检查 everything-linux 的运行依赖
# 用法: ./check-deps.sh /path/to/dist
set -u

DIST="${1:-./dist}"
FAIL=0

ok()   { printf '  [OK]   %s\n' "$*"; }
warn() { printf '  [WARN] %s\n' "$*"; }
bad()  { printf '  [FAIL] %s\n' "$*"; FAIL=$((FAIL+1)); }

ver_ge() {
    # $1 >= $2 : compare dotted version numbers
    printf '%s\n%s\n' "$1" "$2" | sort -V | head -1 | grep -qx "$2"
}

echo "=== everything-linux dependency check ==="
echo "dist: $DIST"

echo
echo "[1] Architecture / kernel"
ARCH="$(uname -m)"
KREL="$(uname -r)"
echo "  arch=$ARCH kernel=$KREL"
case "$ARCH" in
    x86_64|amd64) ok "x86-64" ;;
    *) bad "need x86-64, got $ARCH (must rebuild on target arch)" ;;
esac
KMAJ="${KREL%%.*}"; KMIN="$(echo "$KREL" | cut -d. -f2)"
if [ "$KMAJ" -gt 4 ] || { [ "$KMAJ" -eq 4 ] && [ "$KMIN" -ge 4 ]; }; then
    ok "kernel >= 4.4"
else
    bad "kernel must be >= 4.4"
fi

echo
echo "[2] Static binaries (CLI / daemon)"
for f in everything everythingd; do
    if [ ! -f "$DIST/$f" ]; then bad "$f missing"; continue; fi
    if file "$DIST/$f" 2>/dev/null | grep -q 'static-pie\|statically linked'; then
        ok "$f is static"
    else
        warn "$f is not fully static; check: ldd $DIST/$f"
    fi
    if "$DIST/$f" --help >/dev/null 2>&1; then
        ok "$f runs"
    else
        # daemon --help should exit 0 too
        bad "$f failed to execute (kernel too old? wrong arch?)"
    fi
done

echo
echo "[3] GUI dynamic libraries"
GUI="$DIST/everything-gtk"
if [ ! -f "$GUI" ]; then
    warn "everything-gtk not present (CLI-only deployment is fine)"
else
    if command -v ldd >/dev/null 2>&1; then
        MISSING="$(ldd "$GUI" 2>/dev/null | grep 'not found' || true)"
        if [ -z "$MISSING" ]; then
            ok "all shared libraries resolved"
        else
            bad "missing libraries:"
            echo "$MISSING" | sed 's/^/      /'
        fi
    else
        warn "ldd not available; cannot check shared libraries"
    fi
    if pkg-config --exists gtk+-3.0 2>/dev/null; then
        ok "GTK3 dev files present: $(pkg-config --modversion gtk+-3.0)"
    else
        if ldconfig -p 2>/dev/null | grep -q 'libgtk-3.so.0'; then
            ok "GTK3 runtime present: $(ldconfig -p | grep -m1 'libgtk-3.so.0' | awk '{print $NF}')"
        else
            warn "GTK3 runtime (libgtk-3.so.0) not found"
        fi
    fi
fi

echo
echo "[4] glibc version"
GLIBC="$(ldd --version 2>/dev/null | head -1 | grep -oE '[0-9]+\.[0-9]+' | head -1)"
echo "  glibc=${GLIBC:-unknown}"
if [ -n "$GLIBC" ] && ver_ge "$GLIBC" 2.39; then
    ok "glibc >= 2.39 (matches GUI binary's GLIBC_2.39 weak symbols)"
else
    warn "glibc < 2.39: CLI/daemon unaffected; GUI depends on weak-symbol handling"
fi

echo
echo "[5] Display / tray environment"
echo "  DISPLAY=${DISPLAY:-<unset>}  WAYLAND_DISPLAY=${WAYLAND_DISPLAY:-<unset>}"
if [ -n "${DISPLAY:-}" ] || [ -n "${WAYLAND_DISPLAY:-}" ]; then
    ok "graphical session detected"
else
    warn "no DISPLAY/WAYLAND_DISPLAY; GUI will not start (CLI/daemon still fine)"
fi
if [ -d /usr/share/icons ] || [ -d "${XDG_DATA_HOME:-$HOME/.local/share}/icons" ]; then
    ok "icon theme directories exist"
else
    warn "no icon theme found; tray icon may be blank"
fi

echo
echo "[6] inotify tunables"
for p in /proc/sys/fs/inotify/max_user_watches /proc/sys/fs/inotify/max_queued_events; do
    if [ -r "$p" ]; then echo "  $(basename "$p")=$(cat "$p")"; else warn "cannot read $p"; fi
done

echo
if [ "$FAIL" -eq 0 ]; then
    echo "RESULT: PASS (warnings are non-fatal)"
else
    echo "RESULT: FAIL ($FAIL issue(s))"
fi
exit "$FAIL"
