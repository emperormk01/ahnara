//! Stop command handler

use anyhow::Result;
use std::process::Command;

pub fn handle_stop() -> Result<()> {
    println!("\n🛑 Stopping AHNARA gateway...\n");
    
    // Check if gateway is running
    let check = Command::new("pgrep")
        .arg("-f")
        .arg("ahnara gateway")
        .output();
    
    let was_running = check
        .map(|r| !r.stdout.is_empty())
        .unwrap_or(false);
    
    if !was_running {
        println!("ℹ No running gateway found");
        println!();
        return Ok(());
    }
    
    // Kill gateway process
    let output = Command::new("pkill")
        .arg("-f")
        .arg("ahnara gateway")
        .output();
    
    match output {
        Ok(result) => {
            if result.status.success() {
                println!("✓ Gateway stopped");
            } else {
                println!("⚠ Gateway may not have stopped cleanly");
            }
        }
        Err(e) => {
            println!("⚠ Error: {}", e);
        }
    }
    
    println!();
    Ok(())
}