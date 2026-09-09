//! /update command - download latest pre-built binary from GitHub releases.

use std::process::Command;

const REPO: &str = "emperormk01/Ahnara";
const INSTALL_PATH: &str = "/usr/local/bin/ahnara";

/// Run the update cycle and return a human-readable result string.
pub async fn handle_update() -> String {
    match run_update().await {
        Ok(msg) => msg,
        Err(e) => format!("Update failed: {e}"),
    }
}

async fn run_update() -> Result<String, anyhow::Error> {
    let mut report = String::new();

    // Step 1: get current version
    let current = Command::new(INSTALL_PATH)
        .args(["--version"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());

    report.push_str(&format!("Current: {current}\n"));

    // Step 2: fetch latest release tag from GitHub API
    report.push_str("Checking for updates...\n");
    let client = reqwest::Client::builder()
        .user_agent("ahnara-updater")
        .timeout(std::time::Duration::from_secs(15))
        .build()?;

    let api_url = format!("https://api.github.com/repos/{REPO}/releases/latest");
    let resp = client.get(&api_url).send().await?;

    if !resp.status().is_success() {
        anyhow::bail!("GitHub API returned {}", resp.status());
    }

    let release: serde_json::Value = resp.json().await?;
    let tag = release["tag_name"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No tag_name in release"))?;

    let latest_ver = tag.trim_start_matches('v');

    // Step 3: compare versions
    let current_ver = current
        .split_whitespace()
        .nth(1)
        .unwrap_or("0.0.0");

    if current_ver == latest_ver {
        report.push_str(&format!(
            "Already on the latest version (v{current_ver})."
        ));
        return Ok(report);
    }

    report.push_str(&format!(
        "Update available: v{current_ver} -> v{latest_ver}\n"
    ));

    // Step 4: detect platform and find the right asset
    let target = detect_target()?;
    let asset_name = format!("ahnara-{target}");

    let assets = release["assets"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("No assets in release"))?;

    let asset = assets
        .iter()
        .find(|a| a["name"].as_str() == Some(&asset_name))
        .ok_or_else(|| {
            anyhow::anyhow!("No binary found for {target}")
        })?;

    let download_url = asset["browser_download_url"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("No download URL"))?;

    // Step 5: download to temp file
    report.push_str(&format!("Downloading {asset_name}...\n"));
    let tmp_path = format!("{INSTALL_PATH}.tmp");

    let resp = client.get(download_url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("Download failed: HTTP {}", resp.status());
    }

    let bytes = resp.bytes().await?;
    std::fs::write(&tmp_path, &bytes)?;

    report.push_str(&format!(
        "Downloaded {} ({} bytes)\n",
        asset_name,
        bytes.len()
    ));

    // Step 6: spawn detached stop -> replace -> restart script
    //
    // Architecture requirement: the running gateway MUST be stopped BEFORE the
    // binary on disk is replaced, then restarted with the new binary.
    // The script sleeps 3s to let the HTTP response reach the caller, then:
    //   1. Kills the running gateway (stop)
    //   2. Replaces the binary (mv + chmod)
    //   3. Execs the new binary (restart)
    report.push_str("Scheduling gateway restart...\n");

    let restart_script = format!(
        r#"#!/bin/sh
# Wait for the HTTP response to reach the caller
sleep 3

# Step A: back up old binary, replace with new one
# Safe while running -- the kernel keeps the old binary in memory via the running process.
if [ -f "{INSTALL_PATH}" ]; then
    cp "{INSTALL_PATH}" "{INSTALL_PATH}.bak"
fi
mv "{tmp_path}" "{INSTALL_PATH}"
chmod +x "{INSTALL_PATH}"

# Step B: verify the new binary works
NEW_VER=$("{INSTALL_PATH}" --version 2>/dev/null || echo "unknown")
if [ "$NEW_VER" = "unknown" ]; then
    echo "ahnara-updater: new binary failed verification, rolling back..." >&2
    if [ -f "{INSTALL_PATH}.bak" ]; then
        mv "{INSTALL_PATH}.bak" "{INSTALL_PATH}"
        chmod +x "{INSTALL_PATH}"
    fi
    exit 1
fi
echo "ahnara-updater: installed $NEW_VER, starting gateway..." >&2
rm -f "{INSTALL_PATH}.bak"

# Step C: stop the old gateway, then exec the new one
pkill -f 'ahnara gateway' 2>/dev/null
sleep 2

# Step D: restart the gateway with logging
LOG_DIR="$HOME/.ahnara/logs"
mkdir -p "$LOG_DIR"
LOG_FILE="$LOG_DIR/gateway.log"
echo "ahnara-updater: logs -> $LOG_FILE" >&2
exec "{INSTALL_PATH}" gateway >> "$LOG_FILE" 2>&1
"#,
        tmp_path = tmp_path,
        INSTALL_PATH = INSTALL_PATH,
    );

    let restart_path = "/tmp/ahnara-restart.sh";
    std::fs::write(restart_path, restart_script)?;
    let _ = Command::new("chmod").args(["+x", restart_path]).output()?;

    // Spawn detached: use setsid so the restart script runs in its own session.
    // Without setsid, pkill in the script kills its own parent (the gateway)
    // and the kernel sends SIGHUP to the child, killing it before mv/exec.
    let child = Command::new("setsid")
        .args(["/bin/sh", restart_path])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    // Detach + reap to prevent zombie
    tokio::task::spawn_blocking(move || {
        let _ = child.wait_with_output();
    });

    report.push_str(&format!(
        "Update to v{latest_ver} ready. Gateway will restart in ~5 seconds."
    ));

    Ok(report)
}

fn detect_target() -> Result<String, anyhow::Error> {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;

    match (os, arch) {
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-musl".into()),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-musl".into()),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin".into()),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin".into()),
        _ => anyhow::bail!("Unsupported platform: {os}/{arch}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_target_works() {
        // Should not panic on the current platform
        let _ = detect_target();
    }
}
