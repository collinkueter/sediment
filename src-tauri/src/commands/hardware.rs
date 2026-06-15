//! Onboarding state. ADR-0009 removed the hardware-tier strategy; first-run is
//! now just "set up your engine", so this module keeps only the persisted
//! onboarding-complete flag.

use crate::core::formation_state::AppConfig;
use crate::error::AppResult;
use serde::Serialize;

/// Onboarding state read by the React side on launch.
#[derive(Debug, Serialize)]
pub struct OnboardingState {
    pub complete: bool,
}

#[tauri::command]
pub fn get_onboarding_state(app: tauri::AppHandle) -> AppResult<OnboardingState> {
    let config = AppConfig::load(&app);
    Ok(OnboardingState {
        complete: config.onboarding_complete,
    })
}

#[tauri::command]
pub fn complete_onboarding(app: tauri::AppHandle) -> AppResult<()> {
    let mut config = AppConfig::load(&app);
    config.onboarding_complete = true;
    config.save(&app)
}
