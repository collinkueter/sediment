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

/// Hidden `--mcp-stdio` entry point — the graph-only stdio MCP server
/// (ADR-0009 §5, plan M2). The app spawns *itself* with this flag so the
/// Claude Code CLI can be pointed at it via `--mcp-config` (M3).
///
/// The formation root comes from `SEDIMENT_FORMATION`; the turn's provenance
/// chat id from `SEDIMENT_SOURCE_CHAT_ID` (defaulting to `chat_message:mcp`).
///
/// CRITICAL: this path speaks JSON-RPC on stdout — diagnostics go to stderr
/// only, and the Tauri builder is never reached.
pub fn run_mcp_stdio() -> std::process::ExitCode {
    let formation = match std::env::var("SEDIMENT_FORMATION") {
        Ok(v) if !v.trim().is_empty() => std::path::PathBuf::from(v),
        _ => {
            eprintln!("sediment --mcp-stdio: SEDIMENT_FORMATION env var must be set");
            return std::process::ExitCode::FAILURE;
        }
    };
    let source_chat_id = std::env::var("SEDIMENT_SOURCE_CHAT_ID")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "chat_message:mcp".to_string());

    // Which search backend `search_notes` uses (semantic vs keyword), forwarded
    // by the engine's MCP env block. Defaults to semantic (Ollama).
    let embedding_provider = core::embedding::EmbeddingProvider::from_config(
        std::env::var("SEDIMENT_EMBEDDING_PROVIDER").ok().as_deref(),
    );

    // The custom Ollama endpoint (Docker/Podman/remote), forwarded by the engine's
    // MCP env block. `None` (or blank) keeps the local default. Reads the env var
    // directly — the parent already resolved config-vs-env precedence.
    let ollama_url = core::ollama_sidecar::resolved_endpoint(None);

    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!("sediment --mcp-stdio: could not start async runtime: {e}");
            return std::process::ExitCode::FAILURE;
        }
    };

    match runtime.block_on(core::formation_mcp::serve_stdio(
        formation,
        source_chat_id,
        embedding_provider,
        ollama_url,
    )) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sediment --mcp-stdio: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_notification::init())
        .manage(core::formation_state::FormationState::default())
        .manage(core::memory::MemoryHandle::default())
        .manage(core::watcher::FormationWatcher::default())
        .manage(core::ollama_sidecar::OllamaSidecar::default())
        .manage(core::copilot::CopilotEngineHandle::default())
        .manage(core::cancel::CancelRegistry::default())
        .manage(core::session::SessionRegistry::default())
        .setup(|app| {
            // Logging guard is owned by the app so the appender keeps draining.
            let guard = init_logging(app.handle()).expect("init logging");
            app.manage(LoggingGuard(guard));
            // Point the shared Ollama sidecar at the user's endpoint (Docker/
            // Podman/remote) if one is configured, so the indexer and Ollama
            // commands talk to it instead of auto-spawning a local daemon.
            {
                let cfg = core::formation_state::AppConfig::load(app.handle());
                let endpoint = core::ollama_sidecar::resolved_endpoint(cfg.ollama_url);
                app.state::<core::ollama_sidecar::OllamaSidecar>()
                    .set_endpoint(endpoint);
            }
            // The background indexer needs an AppHandle, only available here.
            let indexer = core::indexer::Indexer::start(app.handle().clone());
            app.manage(indexer);
            // The reminder scheduler (ADR-0007) likewise spawns from here.
            core::reminders::spawn(app.handle().clone());
            tracing::info!("Sediment starting up");
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::app::app_version,
            commands::formation::pick_formation_dir,
            commands::formation::pick_directory,
            commands::formation::open_formation,
            commands::formation::restore_last_formation,
            commands::formation::list_notes,
            commands::formation::read_note,
            commands::formation::write_note,
            commands::memory::index_formation,
            commands::chat::chat_turn,
            commands::chat::cancel_turn,
            commands::chat::get_working_set,
            commands::chat::get_self_summary,
            commands::chat::list_copilot_models,
            commands::chat::dismiss_open_loop,
            commands::session::session_start,
            commands::session::session_push_segment,
            commands::session::session_push_note,
            commands::session::session_rename_speaker,
            commands::session::session_stop,
            commands::audit::list_audit,
            commands::audit::undo_turn,
            commands::audit::undo_fact,
            commands::audit::undo_task_completion,
            commands::tasks::list_tasks,
            commands::tasks::complete_task,
            commands::tasks::snooze_task,
            commands::ollama::ollama_status,
            commands::ollama::ollama_ensure_running,
            commands::ollama::ollama_list_models,
            commands::hardware::get_onboarding_state,
            commands::hardware::complete_onboarding,
            commands::models::check_model_readiness,
            commands::models::pull_ollama_model,
            commands::models::download_bundled_model,
            commands::models::import_bundled_model,
            commands::settings::get_models_dir,
            commands::settings::set_models_dir,
            commands::settings::get_embedding_provider,
            commands::settings::set_embedding_provider,
            commands::settings::get_ollama_url,
            commands::settings::set_ollama_url,
            commands::settings::detect_claude_code,
            commands::settings::detect_copilot,
            commands::settings::get_conversation_engine,
            commands::settings::set_conversation_engine,
            commands::settings::get_agent_tone,
            commands::settings::set_agent_tone,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Sediment");
}

struct LoggingGuard(#[allow(dead_code)] tracing_appender::non_blocking::WorkerGuard);
