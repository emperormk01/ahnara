#!/usr/bin/env bash
set -euo pipefail

# Watchdog for ahnara gateway.
# Runs on a cron (every minute).  Restarts the gateway if it's down.
# Also handles post-reset recovery: binary reinstall + data dir symlink.

LOCKFILE="/tmp/ahnara_watchdog.lock"
LOGFILE="/tmp/ahnara_watchdog.log"
REPO="emperormk01/Ahnara"
BINARY="ahnara"
INSTALL_DIR="/usr/local/bin"
INSTALL_PATH="${INSTALL_DIR}/${BINARY}"

exec 9>"$LOCKFILE"
if ! flock -n 9; then
  exit 0
fi

log() {
    printf '%s %s\n' "$(date -u +"%Y-%m-%dT%H:%M:%SZ")" "$*" >> "$LOGFILE"
}

detect_platform() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
        Linux)  os="unknown-linux-musl" ;;
        Darwin) os="apple-darwin" ;;
        *) echo "unsupported" ;;
    esac
    case "$arch" in
        x86_64)  arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        *) echo "unsupported" ;;
    esac
    echo "${arch}-${os}"
}

ensure_binary() {
    [ -x "$INSTALL_PATH" ] && return 0

    log "WARN: Binary missing at ${INSTALL_PATH}, reinstalling after container reset"
    local target
    target="$(detect_platform)"
    if [ "$target" = "unsupported-unsupported" ]; then
        log "ERROR: Cannot detect platform for reinstall"
        return 1
    fi

    local url="https://github.com/${REPO}/releases/latest/download/${BINARY}-${target}"
    local tmp="/tmp/${BINARY}.watchdog.$$"

    if ! curl -fsSL "$url" -o "$tmp" 2>/dev/null; then
        log "ERROR: Failed to download binary from ${url}"
        rm -f "$tmp"
        return 1
    fi

    chmod +x "$tmp"
    if ! "$tmp" --version >/dev/null 2>&1; then
        log "ERROR: Downloaded binary failed verification"
        rm -f "$tmp"
        return 1
    fi

    mkdir -p "$INSTALL_DIR"
    mv "$tmp" "$INSTALL_PATH"
    log "REINSTALLED: $($INSTALL_PATH --version 2>/dev/null)"
    return 0
}

# ── Data dir recovery (same logic as entrypoint) ─────────────────────────────

find_persistent_root() {
    if [ -n "${ahnara_HOME:-}" ]; then
        echo ""
        return
    fi
    local root_dev
    root_dev="$(stat -c %d / 2>/dev/null || echo 0)"
    local candidates="/home/workspace /data /mnt/data /srv/ahnara /persistent"
    for mount in $candidates; do
        [ -d "$mount" ] || continue
        [ -w "$mount" ] || continue
        local mount_dev
        mount_dev="$(stat -c %d "$mount" 2>/dev/null || echo 0)"
        if [ "$mount_dev" != "$root_dev" ]; then
            echo "$mount"
            return
        fi
    done
    for mount in /home/workspace; do
        if [ -d "$mount" ] && [ -w "$mount" ]; then
            echo "$mount"
            return
        fi
    done
    echo ""
}

ensure_data_dir() {
    local home_auxlo="${HOME}/.ahnara"

    if [ -n "${ahnara_HOME:-}" ]; then
        mkdir -p "$ahnara_HOME"
        if [ ! -e "$home_auxlo" ]; then
            ln -sf "$ahnara_HOME" "$home_auxlo"
        fi
        return
    fi

    [ -L "$home_auxlo" ] && return

    local persistent_root
    persistent_root="$(find_persistent_root)"
    [ -z "$persistent_root" ] && { mkdir -p "$home_auxlo"; return; }

    local data_target="${persistent_root}/.ahnara-data"
    mkdir -p "$data_target"
    if [ -d "$home_auxlo" ] && [ ! -L "$home_auxlo" ]; then
        cp -an "$home_auxlo"/. "$data_target"/ 2>/dev/null || true
        rm -rf "$home_auxlo"
    fi
    ln -sf "$data_target" "$home_auxlo"
    log "Data dir recovered: ${home_auxlo} -> ${data_target}"
}

# ── Main ─────────────────────────────────────────────────────────────────────

ensure_binary
ensure_data_dir

gateway_up() {
    timeout 12s ahnara status 2>/dev/null | grep -q "Gateway:"
}

if gateway_up; then
  log "OK: gateway already running"
  exit 0
fi

log "WARN: gateway down, restarting"
ahnara stop >/dev/null 2>&1 || true
ahnara gateway >/tmp/ahnara_gateway.out 2>&1 &

for _ in {1..12}; do
  sleep 1
  if gateway_up; then
    log "RECOVERED: gateway restarted"
    exit 0
  fi
done

log "FAIL: restart attempted but gateway is still down"
exit 1
