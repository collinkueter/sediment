use crate::core::formation_state::AppConfig;
use crate::core::hardware::{self, HardwareInfo};
use crate::error::AppResult;
use serde::Serialize;

#[tauri::command]
pub fn detect_hardware() -> AppResult<HardwareInfo> {
    Ok(hardware::detect())
}

/// Combined onboarding state read by the React side on launch.
#[derive(Debug, Serialize)]
pub struct OnboardingState {
    pub complete: bool,
    pub selected_tier: Option<String>,
}

#[tauri::command]
pub fn get_onboarding_state(app: tauri::AppHandle) -> AppResult<OnboardingState> {
    let config = AppConfig::load(&app);
    Ok(OnboardingState {
        complete: config.onboarding_complete,
        selected_tier: config.selected_tier,
    })
}

#[tauri::command]
pub fn complete_onboarding(tier: String, app: tauri::AppHandle) -> AppResult<()> {
    let mut config = AppConfig::load(&app);
    config.onboarding_complete = true;
    config.selected_tier = Some(tier);
    config.save(&app)
}
