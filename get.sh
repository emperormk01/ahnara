#!/usr/bin/env bash
set -euo pipefail

REPO="emperormk01/Ahnara"
BINARY="ahnara"
INSTALL_DIR="/usr/local/bin"

detect_platform() {
    local os arch
    os="$(uname -s)"
    arch="$(uname -m)"
    case "$os" in
        Linux)  os="unknown-linux-musl" ;;
        Darwin) os="apple-darwin" ;;
        *) echo "Unsupported OS: $os"; exit 1 ;;
    esac
    case "$arch" in
        x86_64)  arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        *) echo "Unsupported arch: $arch"; exit 1 ;;
    esac
    echo "${arch}-${os}"
}

ensure_npm() {
    if command -v npm &>/dev/null; then
        return
    fi
    echo "==> npm not found. Installing Node.js..."
    if command -v apt-get &>/dev/null; then
        curl -fsSL https://deb.nodesource.com/setup_22.x | bash -
        apt-get install -y nodejs
    elif command -v brew &>/dev/null; then
        brew install node
    else
        echo "Warning: Could not install Node.js automatically. Please install it manually (https://nodejs.org)."
        return 1
    fi
}

# Resolve the pip command, installing pip if needed
ensure_pip() {
    if command -v pip3 &>/dev/null; then
        PIP=pip3
        return
    elif command -v pip &>/dev/null; then
        PIP=pip
        return
    fi
    echo "==> pip not found. Installing pip..."
    if command -v apt-get &>/dev/null; then
        apt-get update -qq && apt-get install -y -qq python3-pip >/dev/null 2>&1
        if command -v pip3 &>/dev/null; then
            PIP=pip3
            return
        fi
    fi
    if command -v python3 &>/dev/null; then
        python3 -m ensurepip --upgrade 2>/dev/null || python3 -m ensurepip 2>/dev/null
        if command -v pip3 &>/dev/null; then
            PIP=pip3
            return
        fi
        # ensurepip may not put pip on PATH; try direct invocation
        if python3 -m pip --version &>/dev/null; then
            PIP="python3 -m pip"
            return
        fi
    fi
    echo "Warning: Could not install pip automatically. Please install pip manually."
    return 1
}

main() {
    local target
    target="$(detect_platform)"
    local url="https://github.com/${REPO}/releases/latest/download/${BINARY}-${target}"

    echo "Downloading ${BINARY} for ${target}..."
    curl -fsSL "$url" -o "/tmp/${BINARY}"
    chmod +x "/tmp/${BINARY}"

    echo "Installing to ${INSTALL_DIR}..."
    mv "/tmp/${BINARY}" "${INSTALL_DIR}/${BINARY}"

    echo "Installed: $(${BINARY} --version)"

    # Install agent-browser (Vercel) if missing
    if ! command -v agent-browser &>/dev/null; then
        echo "Installing agent-browser (by Vercel)..."
        if ensure_npm; then
            npm install -g agent-browser \
                && agent-browser install --with-deps \
                || echo "Warning: agent-browser install failed (manual: npm install -g agent-browser && agent-browser install --with-deps)"
        else
            echo "Warning: agent-browser install failed (manual: npm install -g agent-browser && agent-browser install --with-deps)"
        fi
    fi

    # Install webserp (multi-engine web search, no API key)
    if ! command -v webserp &>/dev/null; then
        echo "Installing webserp (web search engine)..."
        if ensure_pip; then
            $PIP install webserp --break-system-packages -q 2>&1 | tail -5 || echo "Warning: webserp install failed (manual: pip install webserp)"
        else
            echo "Warning: webserp install failed -- pip not available (manual: pip install webserp)"
        fi
    fi

    # Install scrapling (stealth web fetching with anti-bot bypass)
    if ! command -v scrapling &>/dev/null; then
        echo "Installing scrapling (stealth web fetcher)..."
        if ensure_pip; then
            $PIP install 'scrapling[all]>=0.4.7' --break-system-packages -q 2>&1 | tail -5 \
                && scrapling install \
                || echo "Warning: scrapling install failed (manual: pip install 'scrapling[all]>=0.4.7' && scrapling install)"
        else
            echo "Warning: scrapling install failed -- pip not available (manual: pip install 'scrapling[all]>=0.4.7' && scrapling install)"
        fi
    fi

    # Install playwright (required by scrapling stealth/dynamic modes)
    if ! python3 -c "import playwright" 2>/dev/null; then
        echo "Installing playwright (browser automation)..."
        if ensure_pip; then
            $PIP install playwright --break-system-packages -q 2>&1 | tail -5 \
                || echo "Warning: playwright install failed (manual: pip install playwright)"
        else
            echo "Warning: playwright install failed -- pip not available (manual: pip install playwright)"
        fi
    fi

    # Download Chromium for Playwright (if not already installed)
    if python3 -c "import playwright" 2>/dev/null; then
        if ! python3 -c "
from playwright.sync_api import sync_playwright
with sync_playwright() as p:
    b = p.chromium
" 2>/dev/null; then
            echo "Downloading Chromium for Playwright..."
            python3 -m playwright install --with-deps chromium 2>&1 | tail -5 \
                || echo "Warning: Chromium download failed (manual: playwright install --with-deps chromium)"
        fi
    fi


    local HELPER_DIR="/usr/local/share/ahnara"

    # Install faster-whisper (local audio transcription)
    if ! python3 -c "import faster_whisper" 2>/dev/null; then
        echo "Installing faster-whisper (audio transcription)..."
        if ensure_pip; then
            $PIP install faster-whisper --break-system-packages -q 2>&1 | tail -5                 || echo "Warning: faster-whisper install failed (manual: pip install faster-whisper)"
        else
            echo "Warning: faster-whisper install failed -- pip not available (manual: pip install faster-whisper)"
        fi
    fi

    # Pre-download Whisper base model (~150MB) so first transcription is instant
    if python3 -c "import faster_whisper" 2>/dev/null; then
        if ! python3 -c "
from faster_whisper import WhisperModel
import os, glob
cache = os.path.expanduser('~/.cache/huggingface/hub')
if os.path.isdir(cache):
    matches = glob.glob(os.path.join(cache, '*whisper*base*'))
    if matches:
        raise SystemExit(0)
raise SystemExit(1)
" 2>/dev/null; then
            local avail_kb
            avail_kb=$(df -k "${HOME:-/root}" 2>/dev/null | awk 'NR==2{print $4}')
            if [ "${avail_kb:-0}" -lt 204800 ]; then
                echo "Warning: Less than 200MB free -- skipping Whisper model pre-download"
                echo "  Model will auto-download on first transcription attempt"
            else
                echo "Pre-downloading Whisper base model (~150MB)..."
                python3 -c "from faster_whisper import WhisperModel; WhisperModel('base', device='cpu', compute_type='int8')" 2>&1 | tail -3                     || echo "Warning: Whisper model download failed (will retry on first use)"
            fi
        fi
    fi

    # Deploy transcribe helper script
    local TRANSCRIBE_SCRIPT="${HELPER_DIR}/transcribe.py"
    if [ ! -f "$TRANSCRIBE_SCRIPT" ]; then
        echo "Deploying transcribe helper script..."
        mkdir -p "$HELPER_DIR"
        curl -fsSL "https://raw.githubusercontent.com/emperormk01/Ahnara/master/scripts/transcribe.py" \
            -o "$TRANSCRIBE_SCRIPT" \
            && chmod +x "$TRANSCRIBE_SCRIPT" \
            || echo "Warning: Failed to download transcribe helper script"
    fi

    # Deploy stealth_fetch helper script
    local HELPER_PATH="${HELPER_DIR}/stealth_fetch_helper.py"
    if [ ! -f "$HELPER_PATH" ]; then
        echo "Deploying stealth_fetch helper script..."
        mkdir -p "$HELPER_DIR"
        curl -fsSL "https://raw.githubusercontent.com/emperormk01/Ahnara/master/scripts/stealth_fetch_helper.py" \
            -o "$HELPER_PATH" \
            && chmod +x "$HELPER_PATH" \
            || echo "Warning: Failed to download stealth_fetch helper script"
    fi

    # Deploy watchdog script (auto-restarts gateway if it crashes)
    local WATCHDOG_BIN="/usr/local/bin/ahnara_watchdog.sh"
    echo "Deploying watchdog script..."
    curl -fsSL "https://raw.githubusercontent.com/emperormk01/Ahnara/master/scripts/ahnara_watchdog.sh" \
        -o "$WATCHDOG_BIN" \
        && chmod +x "$WATCHDOG_BIN" \
        || echo "Warning: Failed to deploy watchdog script"

    # Deploy self-healing entrypoint (auto-reinstalls binary on container reset)
    local ENTRYPOINT_BIN="/usr/local/bin/ahnara_entrypoint.sh"
    echo "Deploying self-healing entrypoint..."
    curl -fsSL "https://raw.githubusercontent.com/emperormk01/Ahnara/master/scripts/ahnara_entrypoint.sh" \
        -o "$ENTRYPOINT_BIN" \
        && chmod +x "$ENTRYPOINT_BIN" \
        || echo "Warning: Failed to deploy entrypoint script"

    echo "Browser: agent-browser (by Vercel)"
    echo "Web search: webserp (multi-engine, no API key)"
    echo "Stealth fetch: scrapling (anti-bot bypass, TLS fingerprint spoofing)"
    echo "Audio transcription: faster-whisper (local Whisper model)"

    # Beginner-friendly onboarding. Many users install via curl | bash and have
    # no idea what to run next. Spell it out.
    cat <<'EOF'

✓ ahnara is ready.

  3 steps to your first chat (about 2 minutes):

    1.  ahnara setup
        (Interactive wizard -- pick a provider, paste your API key)

    2.  ahnara gateway
        (Start the server in the background; prints when ready)

    3.  ahnara chat "hello"
        (Talk to the agent from your terminal)

  Or chat immediately with the free default provider (no setup needed):

    export NVIDIA_API_KEY=your-key-from-build.nvidia.com
    ahnara setup --provider nvidia --api-key "$NVIDIA_API_KEY"
    ahnara chat "hello"

  Connect Telegram:
    - Open Telegram, message @BotFather, send /newbot, copy the token
    - Run: ahnara token set TELEGRAM_BOT_TOKEN <your-token>
    - Restart: ahnara gateway
    - Send /start to your new bot

  Docs: https://github.com/emperormk01/Ahnara
  Report issues: https://github.com/emperormk01/Ahnara/issues

EOF
}

main "$@"
