#!/usr/bin/env bash
set -euo pipefail

# Self-healing entrypoint for ahnara.
#
# Solves TWO problems on container runtime resets:
#   1. Binary at /usr/local/bin/ahnara gets wiped -> auto-reinstall
#   2. User data at ~/.ahnara gets wiped           -> auto-persist via symlink
#
# Zero-config: detects persistent storage automatically.
# Works on Zo, Docker, K8s, bare metal -- any container runtime.

REPO="emperormk01/Ahnara"
BINARY="ahnara"
INSTALL_DIR="/usr/local/bin"
INSTALL_PATH="${INSTALL_DIR}/${BINARY}"
LOCKFILE="/tmp/ahnara_entrypoint.lock"

log() {
    printf '[ahnara] %s %s\n' "$(date -u +'%Y-%m-%dT%H:%M:%SZ')" "$*" >&2
}

# ── Binary recovery ──────────────────────────────────────────────────────────

detect_platform() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
        Linux)  os="unknown-linux-musl" ;;
        Darwin) os="apple-darwin" ;;
        *) log "Unsupported OS: $os"; exit 1 ;;
    esac
    case "$arch" in
        x86_64)  arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        *) log "Unsupported arch: $arch"; exit 1 ;;
    esac
    echo "${arch}-${os}"
}

ensure_binary() {
    [ -x "$INSTALL_PATH" ] && return 0

    log "Binary missing at ${INSTALL_PATH} -- reinstalling after container reset"

    exec 9>"$LOCKFILE"
    if ! flock -n 9; then
        log "Another install in progress, waiting..."
        flock 9
        [ -x "$INSTALL_PATH" ] && return 0
    fi

    local target url tmp
    target="$(detect_platform)"
    url="https://github.com/${REPO}/releases/latest/download/${BINARY}-${target}"
    tmp="/tmp/${BINARY}.install.$$"

    log "Downloading ${BINARY} for ${target}..."
    if ! curl -fsSL "$url" -o "$tmp"; then
        log "ERROR: Download failed from ${url}"
        rm -f "$tmp"
        exit 1
    fi

    chmod +x "$tmp"
    if ! "$tmp" --version >/dev/null 2>&1; then
        log "ERROR: Downloaded binary failed --version check"
        rm -f "$tmp"
        exit 1
    fi

    mkdir -p "$INSTALL_DIR"
    mv "$tmp" "$INSTALL_PATH"
    log "Installed: $($INSTALL_PATH --version)"
}

# ── Data persistence ─────────────────────────────────────────────────────────
#
# On container reset, ~/.ahnara (config, memory DB, reflections, tokens,
# sessions) is wiped if it lives on the ephemeral root filesystem.
#
# Fix: symlink ~/.ahnara to a directory on the first persistent mount we
# find.  The binary keeps writing to ~/.ahnara -- it doesn't know or care
# that it's a symlink.  No code changes needed in the Rust binary.
#
# Detection order:
#   1. ahnara_HOME env var (explicit user override -- highest priority)
#   2. Already a symlink?  User configured it themselves -- leave it alone.
#   3. Probe well-known persistent mount points:
#        /home/workspace  (Zo Computer)
#        /data            (Docker convention)
#        /mnt/data        (K8s PVC common)
#        /srv/ahnara   (FHS-compliant)
#        /persistent      (generic)
#      First one that is writable AND on a different device than / wins.
#   4. Fall back to $HOME/.ahnara as-is (bare metal, persistent home).

find_persistent_root() {
    # If user explicitly set ahnara_HOME, we still symlink but to that target.
    # However if they set it, they likely manage persistence themselves -- skip.
    if [ -n "${ahnara_HOME:-}" ]; then
        echo ""
        return
    fi

    local root_dev
    root_dev="$(stat -c %d / 2>/dev/null || echo 0)"

    local candidates="/home/workspace /data /mnt/data /srv/ahnara /persistent"
    for mount in $candidates; do
        [ -d "$mount" ] || continue
        # Check it's writable
        [ -w "$mount" ] || continue
        # Check it's on a different filesystem than /  (i.e. a volume mount)
        local mount_dev
        mount_dev="$(stat -c %d "$mount" 2>/dev/null || echo 0)"
        if [ "$mount_dev" != "$root_dev" ]; then
            echo "$mount"
            return
        fi
    done

    # Same-device fallback: if /home/workspace exists and is writable, prefer it
    # even if it's the same device (Zo puts workspace on the root 9p fs but it
    # IS persistent).
    for mount in /home/workspace; do
        if [ -d "$mount" ] && [ -w "$mount" ]; then
            echo "$mount"
            return
        fi
    done

    echo ""
}

persist_data_dir() {
    local home_auxlo="${HOME}/.ahnara"

    # Explicit override: user manages their own persistence
    if [ -n "${ahnara_HOME:-}" ]; then
        mkdir -p "$ahnara_HOME"
        if [ ! -e "$home_auxlo" ]; then
            ln -sfn "$ahnara_HOME" "$home_auxlo"
            log "Data dir: ${ahnara_HOME} (ahnara_HOME)"
        elif [ -d "$home_auxlo" ] && [ ! -L "$home_auxlo" ]; then
            # Migrate existing data into the user-specified location
            cp -an "$home_auxlo"/. "$ahnara_HOME"/ 2>/dev/null || true
            rm -rf "$home_auxlo"
            ln -sfn "$ahnara_HOME" "$home_auxlo"
            log "Migrated data -> ${ahnara_HOME} (ahnara_HOME)"
        fi
        return
    fi

    # Already a symlink?  User or a previous run configured it -- leave it.
    if [ -L "$home_auxlo" ]; then
        return
    fi

    local persistent_root
    persistent_root="$(find_persistent_root)"

    if [ -z "$persistent_root" ]; then
        # No persistent mount found -- bare metal or simple container.
        # ~/.ahnara is on the root fs which is presumably persistent.
        mkdir -p "$home_auxlo"
        return
    fi

    local data_target="${persistent_root}/.ahnara-data"

    if [ -d "$home_auxlo" ] && [ ! -L "$home_auxlo" ]; then
        # Existing data on ephemeral fs -- migrate to persistent storage
        mkdir -p "$data_target"
        # Only copy if target is empty (first migration)
        if [ -z "$(ls -A "$data_target" 2>/dev/null)" ]; then
            cp -a "$home_auxlo"/. "$data_target"/ 2>/dev/null || true
            log "Migrated data: ${home_auxlo} -> ${data_target}"
        else
            # Target already has data (previous run) -- merge (don't overwrite)
            cp -an "$home_auxlo"/. "$data_target"/ 2>/dev/null || true
            log "Merged data: ${home_auxlo} -> ${data_target}"
        fi
        rm -rf "$home_auxlo"
        ln -sfn "$data_target" "$home_auxlo"
        log "Data dir symlinked: ${home_auxlo} -> ${data_target}"
    else
        # Fresh install -- just create the symlink
        mkdir -p "$data_target"
        ln -sfn "$data_target" "$home_auxlo"
        log "Data dir: ${home_auxlo} -> ${data_target}"
    fi
}

# ── Main ─────────────────────────────────────────────────────────────────────

ensure_binary
persist_data_dir

exec "$INSTALL_PATH" "$@"
