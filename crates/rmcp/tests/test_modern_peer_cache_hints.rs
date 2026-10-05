//! Guard: a server that advertises `2026-07-28` must put `ttlMs` + `cacheScope`
//! on its list/read results for peers that negotiated it (SEP-2549). Handlers
//! built with `ListToolsResult::with_all_items(..)` (every iris/mcps server)
//! never set them, so the SDK fills the conservative default itself.
#![cfg(not(feature = "local"))]
#![cfg(feature = "client")]

use rmcp::{
    ClientHandler, RoleServer, ServerHandler, ServiceExt,
    model::{
        CacheScope, ClientInfo, ErrorData, ListToolsResult, PaginatedRequestParams,
        ProtocolVersion, ServerResult, Tool,
    },
    service::RequestContext,
};
use serde_json::json;

#[derive(Debug, Clone, Default)]
struct PlainServer;

impl ServerHandler for PlainServer {
    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        // The shape every in-tree server uses: no cache hints set.
        Ok(ListToolsResult::with_all_items(vec![Tool::new(
            "echo",
            "echo",
            std::sync::Arc::new(serde_json::Map::new()),
        )]))
    }
}

#[derive(Debug, Clone)]
struct VersionedClient(ProtocolVersion);

impl ClientHandler for VersionedClient {
    fn get_info(&self) -> ClientInfo {
        let mut info = ClientInfo::default();
        info.protocol_version = self.0.clone();
        info
    }
}

async fn list_tools_at(version: ProtocolVersion) -> ListToolsResult {
    let (server_io, client_io) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        PlainServer.serve(server_io).await?.waiting().await?;
        anyhow::Ok(())
    });
    let client = VersionedClient(version)
        .serve(client_io)
        .await
        .expect("client should connect");
    let result = client.list_tools(None).await.expect("tools/list");
    client.cancel().await.expect("cancel");
    server.await.expect("server task").expect("server");
    result
}

/// If the default supported set contains 2026-07-28, a modern peer's
/// `tools/list` MUST carry both cache hints. This is the invariant whose
/// violation made Claude Code register 0 tools.
#[tokio::test]
async fn advertising_2026_07_28_implies_tools_list_carries_cache_hints() {
    assert!(
        PlainServer
            .supported_protocol_versions()
            .contains(&ProtocolVersion::V_2026_07_28),
        "default no longer advertises 2026-07-28; this guard can be retired"
    );
    let result = list_tools_at(ProtocolVersion::V_2026_07_28).await;
    assert_eq!(
        result.ttl_ms,
        Some(0),
        "ttlMs missing on a 2026-07-28 tools/list"
    );
    assert_eq!(result.cache_scope, Some(CacheScope::Private));
}

/// Legacy peers keep the pre-2026 wire shape: no cache hints injected.
#[tokio::test]
async fn legacy_peer_tools_list_has_no_cache_hints() {
    let result = list_tools_at(ProtocolVersion::V_2025_11_25).await;
    assert_eq!(result.ttl_ms, None);
    assert_eq!(result.cache_scope, None);
}

/// A handler's own choice wins over the default.
#[test]
fn explicit_cache_hints_are_preserved() {
    let mut result = ServerResult::ListToolsResult(
        ListToolsResult::default()
            .with_ttl_ms(5_000)
            .with_cache_scope(CacheScope::Public),
    );
    result.fill_default_cache_hints_for_modern_peer();
    let value = serde_json::to_value(&result).unwrap();
    assert_eq!(value["ttlMs"], json!(5000));
    assert_eq!(value["cacheScope"], json!("public"));
}

/// Non-cacheable results are untouched.
#[test]
fn call_results_are_not_given_cache_hints() {
    let mut result = ServerResult::CallToolResult(rmcp::model::CallToolResult::success(vec![]));
    result.fill_default_cache_hints_for_modern_peer();
    let value = serde_json::to_value(&result).unwrap();
    assert!(value.get("ttlMs").is_none() && value.get("cacheScope").is_none());
}
