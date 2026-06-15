// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() -> std::process::ExitCode {
    // Hidden subcommand: run as the graph-only stdio MCP server (ADR-0009 §5,
    // plan M2). Branch before any Tauri builder code runs — this path speaks
    // JSON-RPC on stdout and must never start the GUI.
    if std::env::args().any(|arg| arg == "--mcp-stdio") {
        return sediment_lib::run_mcp_stdio();
    }

    sediment_lib::run();
    std::process::ExitCode::SUCCESS
}
