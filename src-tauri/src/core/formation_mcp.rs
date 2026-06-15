//! The stdio MCP server — ADR-0009 §5, plan M2.
//!
//! Wraps the transport-agnostic [`formation_tools`] layer (M1) in a Model
//! Context Protocol server spoken over stdin/stdout. The Claude Code CLI is
//! later spawned with `--mcp-config` pointing at this server (M3), so the
//! conversational agent reaches the bi-temporal graph and the embedding index
//! through it. Note read/write is deliberately *not* exposed here — that is
//! Claude Code's own native file tools (ADR-0009 Option B).
//!
//! The server uses rmcp's dynamic [`ServerHandler`] surface: `list_tools`
//! advertises [`formation_tools::tool_schemas`] verbatim and `call_tool` routes
//! through [`formation_tools::dispatch`]. No per-tool structs and no duplicated
//! schemas — `formation_tools` already owns the single source of truth.
//!
//! CRITICAL: the MCP protocol is JSON-RPC framed on stdout. Nothing in this
//! module — or the `--mcp-stdio` subcommand that drives it — may write to
//! stdout. Diagnostics go to stderr only.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CallToolRequestParams, CallToolResult, Content, Implementation, InitializeResult, JsonObject,
    ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServiceExt};

use crate::commands::formation::APP_DIR;
use crate::core::formation_tools::{self, ToolContext};
use crate::core::memory::MemoryStore;
use crate::core::ollama_sidecar::OllamaSidecar;
use crate::error::{AppError, AppResult};

/// The graph store lives at `<formation>/.chat-notes/memory` (mirrors the path
/// `commands::memory` and `formation_tools`' own tests use).
fn memory_dir(formation_root: &Path) -> PathBuf {
    formation_root.join(APP_DIR).join("memory")
}

/// The MCP server: a [`ServerHandler`] over one [`ToolContext`].
///
/// A single context is built per process — V1 spawns one server per turn
/// (ADR-0009 §5), so the store/embedder/provenance are fixed for the server's
/// whole lifetime.
struct FormationMcp {
    ctx: ToolContext,
}

impl FormationMcp {
    /// Open the graph store and assemble the tool context.
    async fn new(formation_root: PathBuf, source_chat_id: String) -> AppResult<Self> {
        let store = MemoryStore::open(&memory_dir(&formation_root)).await?;
        let ctx = ToolContext {
            store,
            formation_root,
            sidecar: OllamaSidecar::default(),
            source_chat_id,
        };
        Ok(Self { ctx })
    }
}

/// Convert one [`formation_tools::ToolSchema`] into an rmcp [`Tool`].
///
/// The schema's `parameters` is already a JSON-Schema object; rmcp's `Tool`
/// wants its input schema as an `Arc<JsonObject>` (a `serde_json::Map`). A
/// schema that is somehow not an object is a programming error in
/// `tool_schemas`, so we fall back to an empty object rather than panic.
fn to_rmcp_tool(schema: &formation_tools::ToolSchema) -> Tool {
    let input_schema: JsonObject = schema
        .parameters
        .as_object()
        .cloned()
        .unwrap_or_default();
    Tool::new(schema.name, schema.description, Arc::new(input_schema))
}

/// The full advertised tool set, as rmcp `Tool`s. Shared by `list_tools` and
/// the unit tests.
fn rmcp_tools() -> Vec<Tool> {
    formation_tools::tool_schemas()
        .iter()
        .map(to_rmcp_tool)
        .collect()
}

impl ServerHandler for FormationMcp {
    fn get_info(&self) -> InitializeResult {
        InitializeResult {
            protocol_version: ProtocolVersion::default(),
            capabilities: ServerCapabilities::builder().enable_tools().build(),
            server_info: Implementation::from_build_env(),
            instructions: Some(
                "Graph-only tools for a Sediment formation: semantic note search, \
                 entity and relationship lookup, bi-temporal contradiction checks, \
                 and the structured record_* writes. Note files are read and written \
                 with the native file tools, not here."
                    .to_string(),
            ),
        }
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(rmcp_tools()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResult, McpError> {
        // rmcp models arguments as an optional JSON object; `dispatch` wants a
        // plain `Value`. An absent arguments map becomes `{}`.
        let args = request
            .arguments
            .map(serde_json::Value::Object)
            .unwrap_or_else(|| serde_json::json!({}));

        match formation_tools::dispatch(&self.ctx, &request.name, args).await {
            // A tool result is returned both as structured content (for clients
            // that parse it) and as a text rendering (the MCP baseline).
            Ok(value) => {
                let text = serde_json::to_string(&value)
                    .unwrap_or_else(|e| format!("{{\"error\":\"serialize result: {e}\"}}"));
                Ok(CallToolResult {
                    content: vec![Content::text(text)],
                    structured_content: Some(value),
                    is_error: Some(false),
                    meta: None,
                })
            }
            // A bad-argument / unknown-tool / store error is surfaced to the
            // agent as a tool-error result — never a server crash (ADR-0009 §5).
            Err(err) => Ok(CallToolResult::error(vec![Content::text(err.to_string())])),
        }
    }
}

/// Run the stdio MCP server until the client disconnects.
///
/// Opens the graph store at `<formation_root>/.chat-notes/memory`, advertises
/// the nine `formation_tools`, and serves JSON-RPC over stdin/stdout. Returns
/// when the peer closes the connection. `source_chat_id` is stamped as
/// provenance on every Fact recorded during the session.
pub async fn serve_stdio(formation_root: PathBuf, source_chat_id: String) -> AppResult<()> {
    let server = FormationMcp::new(formation_root, source_chat_id).await?;

    let running = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| AppError::other(format!("MCP server failed to start: {e}")))?;

    running
        .waiting()
        .await
        .map_err(|e| AppError::other(format!("MCP server task join error: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tempdir() -> PathBuf {
        let p = std::env::temp_dir()
            .join("sediment-test-formation-mcp")
            .join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir_all(&p).expect("tempdir");
        p
    }

    /// The advertised rmcp tool list is the nine `formation_tools`, each with
    /// a non-empty object input schema.
    #[test]
    fn rmcp_tools_advertises_the_nine_tools() {
        let tools = rmcp_tools();
        assert_eq!(tools.len(), 9, "nine graph tools");

        let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
        for expected in [
            "search_notes",
            "find_entity",
            "related_facts",
            "find_contradiction",
            "record_fact",
            "retract_fact",
            "record_task",
            "record_open_loop",
            "close_open_loop",
        ] {
            assert!(names.contains(&expected), "missing tool {expected}");
        }

        for t in &tools {
            assert!(
                t.description.as_ref().is_some_and(|d| !d.is_empty()),
                "{} has a description",
                t.name
            );
            // The input schema is a JSON-Schema object with declared properties.
            assert_eq!(
                t.input_schema.get("type").and_then(|v| v.as_str()),
                Some("object"),
                "{} input schema is an object",
                t.name
            );
            assert!(
                t.input_schema.contains_key("properties"),
                "{} input schema has properties",
                t.name
            );
        }
    }

    /// `to_rmcp_tool` preserves the underlying `formation_tools` schema verbatim
    /// — names and parameter schemas match one-for-one, no duplication drift.
    #[test]
    fn rmcp_tools_match_formation_tool_schemas() {
        let schemas = formation_tools::tool_schemas();
        let tools = rmcp_tools();
        assert_eq!(schemas.len(), tools.len());
        for (schema, tool) in schemas.iter().zip(tools.iter()) {
            assert_eq!(schema.name, tool.name.as_ref());
            assert_eq!(
                &serde_json::Value::Object(tool.input_schema.as_ref().clone()),
                &schema.parameters,
                "{} input schema round-trips",
                schema.name
            );
        }
    }

    /// `get_info` advertises the tools capability so a client knows to call
    /// `tools/list`.
    #[tokio::test]
    async fn server_info_enables_tools_capability() {
        let root = tempdir();
        let server = FormationMcp::new(root.clone(), "chat_message:test".to_string())
            .await
            .expect("build server");
        let info = server.get_info();
        assert!(
            info.capabilities.tools.is_some(),
            "the tools capability is advertised"
        );
        std::fs::remove_dir_all(root).ok();
    }

    /// The `call_tool` handler routes through `dispatch`: a `record_fact`
    /// followed by a `find_entity` round-trips through the MCP layer against a
    /// temp formation, and structured content carries the JSON result.
    #[tokio::test]
    async fn call_tool_routes_record_and_find_through_dispatch() {
        let root = tempdir();
        let server = FormationMcp::new(root.clone(), "chat_message:test".to_string())
            .await
            .expect("build server");

        // record_fact via dispatch (the exact path call_tool takes).
        let recorded = formation_tools::dispatch(
            &server.ctx,
            "record_fact",
            json!({
                "subject": "Josh", "subject_type": "person",
                "predicate": "works_at",
                "object": "Cloudflare", "object_type": "organization"
            }),
        )
        .await
        .expect("record_fact");
        assert!(recorded["fact_id"].as_str().unwrap().starts_with("fact:"));

        // find_entity via dispatch resolves the subject and its current fact.
        let found = formation_tools::dispatch(
            &server.ctx,
            "find_entity",
            json!({ "name": "Josh" }),
        )
        .await
        .expect("find_entity");
        assert_eq!(found["found"], true);
        let facts = found["current_facts"].as_array().unwrap();
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0]["predicate"], "works_at");

        std::fs::remove_dir_all(root).ok();
    }

    /// A dispatch error becomes an `is_error` tool result (a serializable JSON
    /// payload), not a panic — the error half of the `call_tool` arm.
    #[tokio::test]
    async fn call_tool_error_path_returns_an_error_result() {
        let root = tempdir();
        let server = FormationMcp::new(root.clone(), "chat_message:test".to_string())
            .await
            .expect("build server");

        // An unknown tool name is an Err from dispatch.
        let err = formation_tools::dispatch(&server.ctx, "no_such_tool", json!({})).await;
        assert!(err.is_err(), "unknown tool errors at the dispatch layer");

        // The handler turns that Err into an error CallToolResult.
        let result = CallToolResult::error(vec![Content::text(
            err.unwrap_err().to_string(),
        )]);
        assert_eq!(result.is_error, Some(true));
        assert!(!result.content.is_empty());

        std::fs::remove_dir_all(root).ok();
    }

    /// `serve_stdio` needs a connected MCP client on stdio; exercising it would
    /// require a live peer. Covered manually / by M3's spawn path.
    #[tokio::test]
    #[ignore]
    async fn serve_stdio_runs() {
        let root = tempdir();
        serve_stdio(root.clone(), "chat_message:mcp".to_string())
            .await
            .expect("serve_stdio");
        std::fs::remove_dir_all(root).ok();
    }
}
