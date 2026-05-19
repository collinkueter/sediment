//! Hardware detection: total RAM + (on macOS) Apple Silicon chip family.
//! Maps to the spec §5 tier table so onboarding can pre-fill a recommendation.

use serde::Serialize;
use std::process::Command;
use sysinfo::System;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
pub enum Tier {
    Lite,
    Standard,
    Pro,
    Byok,
}

#[derive(Debug, Clone, Serialize)]
pub struct HardwareInfo {
    pub total_ram_gb: u32,
    /// Best-effort chip identifier (e.g. "Apple M3 Pro", "Apple M1", "Intel Core i7").
    pub chip: String,
    /// Recommended tier given the detected hardware.
    pub recommended_tier: Tier,
}

pub fn detect() -> HardwareInfo {
    let mut sys = System::new_all();
    sys.refresh_memory();
    // sysinfo reports total_memory in bytes since 0.30+.
    let total_ram_gb = ((sys.total_memory() as f64) / 1024.0 / 1024.0 / 1024.0).round() as u32;
    let chip = detect_chip();
    let recommended_tier = score_tier(total_ram_gb, &chip);
    HardwareInfo {
        total_ram_gb,
        chip,
        recommended_tier,
    }
}

#[cfg(target_os = "macos")]
fn detect_chip() -> String {
    // `system_profiler` is universally available on macOS; cheap one-shot.
    let output = Command::new("system_profiler")
        .arg("SPHardwareDataType")
        .output();
    let Ok(output) = output else {
        return "Unknown Mac".to_string();
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    // Prefer "Chip:" (Apple Silicon); fall back to "Processor Name:" (Intel).
    for line in stdout.lines() {
        if let Some(rest) = line.trim().strip_prefix("Chip:") {
            return rest.trim().to_string();
        }
    }
    for line in stdout.lines() {
        if let Some(rest) = line.trim().strip_prefix("Processor Name:") {
            return rest.trim().to_string();
        }
    }
    "Mac".to_string()
}

#[cfg(not(target_os = "macos"))]
fn detect_chip() -> String {
    // Phase 1 is macOS-only; non-mac paths still return something useful.
    "Unknown CPU".to_string()
}

/// Map (RAM, chip) → recommended tier per spec §5.
/// - Apple Silicon Pro/Max/Ultra with ≥48GB → Pro
/// - Any Apple Silicon ≥24GB or strong Intel ≥32GB → Standard
/// - ≥16GB → Lite
/// - Otherwise → BYOK
fn score_tier(ram_gb: u32, chip: &str) -> Tier {
    let chip_lower = chip.to_lowercase();
    let is_apple_silicon = chip_lower.starts_with("apple");
    let is_high_end_silicon = is_apple_silicon
        && (chip_lower.contains("pro")
            || chip_lower.contains("max")
            || chip_lower.contains("ultra"));
    let standard_ram_threshold = if is_apple_silicon { 24 } else { 32 };

    if ram_gb >= 48 && is_high_end_silicon {
        Tier::Pro
    } else if ram_gb >= standard_ram_threshold {
        Tier::Standard
    } else if ram_gb >= 16 {
        Tier::Lite
    } else {
        Tier::Byok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tier_scoring_matches_spec() {
        assert_eq!(score_tier(64, "Apple M3 Max"), Tier::Pro);
        assert_eq!(score_tier(36, "Apple M3 Pro"), Tier::Standard);
        assert_eq!(score_tier(32, "Intel Core i9"), Tier::Standard);
        assert_eq!(score_tier(16, "Apple M1"), Tier::Lite);
        assert_eq!(score_tier(8, "Apple M1"), Tier::Byok);
    }
}
