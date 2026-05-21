// Core module: long-lived state and subsystems.
pub mod cloud;
pub mod diff_gen;
pub mod extraction;
pub mod formation_state;
pub mod hardware;
pub mod indexer;
pub mod intent;
pub mod llm_extractor;
pub mod memory;
pub mod models;
pub mod ollama_sidecar;
pub mod router;
pub mod similarity;
pub mod staging;
pub mod watcher;

// Stubs for later milestones:
// pub mod watcher;         // M4
// pub mod hardware;        // M5
// pub mod ollama_sidecar;  // M6
