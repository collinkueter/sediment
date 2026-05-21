mod commands;
mod core;
mod error;

use tracing_appender::rolling;
use tracing_subscriber::{fmt, prelude::*, EnvFilter};

fn init_logging(
    app_handle: &tauri::AppHandle,
) -> anyhow_lite::Result<tracing_appender::non_blocking::WorkerGuard> {
    let log_dir = app_handle
        .path()
        .app_log_dir()
        .map_err(|e| anyhow_lite::Error::msg(format!("resolve log dir: {e}")))?;
    std::fs::create_dir_all(&log_dir).ok();

    let file_appender = rolling::daily(log_dir, "sediment.log");
    let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = EnvFilter::try_from_env("SEDIMENT_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,sediment_lib=debug"));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer().with_target(true).with_ansi(true))
        .with(fmt::layer().with_writer(non_blocking).with_ansi(false))
        .init();

    Ok(guard)
}

// Tiny inline error helper to avoid pulling anyhow as a top-level dep.
mod anyhow_lite {
    pub type Result<T> = std::result::Result<T, Error>;

    #[derive(Debug)]
    pub struct Error(String);

    impl Error {
        pub fn msg<S: Into<String>>(s: S) -> Self {
            Self(s.into())
        }
    }

    impl std::fmt::Display for Error {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(&self.0)
        }
    }

    impl std::error::Error for Error {}
}

use tauri::Manager;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_fs::init())
        .manage(core::formation_state::FormationState::default())
        .manage(core::memory::MemoryHandle::default())
        .manage(core::watcher::FormationWatcher::default())
        .manage(core::ollama_sidecar::OllamaSidecar::default())
        .setup(|app| {
            // Logging guard is owned by the app so the appender keeps draining.
            let guard = init_logging(app.handle()).expect("init logging");
            app.manage(LoggingGuard(guard));
            // The background indexer needs an AppHandle, only available here.
            let indexer = core::indexer::Indexer::start(app.handle().clone());
            app.manage(indexer);
            tracing::info!("Sediment starting up");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app::app_version,
            commands::formation::pick_formation_dir,
            commands::formation::open_formation,
            commands::formation::restore_last_formation,
            commands::formation::list_notes,
            commands::formation::read_note,
            commands::formation::write_note,
            commands::memory::memory_smoke_test,
            commands::memory::index_note,
            commands::memory::index_formation,
            commands::memory::search_notes,
            commands::memory::relate_fact_command,
            commands::memory::current_facts,
            commands::extraction::extract_entities,
            commands::extraction::extract_and_upsert,
            commands::extraction::extract_facts,
            commands::chat::chat_write,
            commands::chat::chat_ask,
            commands::chat::classify_intent,
            commands::staging::list_staging,
            commands::staging::get_staging,
            commands::staging::discard_staging,
            commands::staging::update_staging,
            commands::staging::resolve_conflict,
            commands::staging::apply_disambiguation,
            commands::staging::dismiss_disambiguation,
            commands::staging::keep_staging,
            commands::staging::undo_commit,
            commands::ollama::ollama_status,
            commands::ollama::ollama_ensure_running,
            commands::ollama::ollama_list_models,
            commands::ollama::ollama_generate,
            commands::hardware::detect_hardware,
            commands::hardware::get_onboarding_state,
            commands::hardware::complete_onboarding,
            commands::models::check_model_readiness,
            commands::models::pull_ollama_model,
            commands::models::download_gliner_model,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Sediment");
}

struct LoggingGuard(#[allow(dead_code)] tracing_appender::non_blocking::WorkerGuard);
