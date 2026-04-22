//! Forward-looking scaffold for [SEP-2243](https://modelcontextprotocol.io/seps/2243-http-standardization)
//! (Draft, post 2025-11-25).
//!
//! SEP-2243 adds two HTTP-level conventions to Streamable HTTP transport:
//!
//! - **Standard request headers** `Mcp-Method` (JSON-RPC method name) and
//!   `Mcp-Name` (tool / resource / prompt name) on every POST. Purely
//!   observational - load balancers, rate limiters, and WAFs can route /
//!   throttle on them without parsing the JSON body.
//! - **`x-mcp-header` tool-parameter pass-through**: a tool may declare a
//!   parameter that, at call time, is promoted to an outbound HTTP header so
//!   autonomous pipelines can thread per-call authorization, tracing, or
//!   correlation headers without bespoke transport middleware.
//!
//! rmcp core does not implement SEP-2243 yet (the SEP is still draft; the
//! transport plumbing lands when it finalizes). This module ships the
//! low-risk pieces that are useful today:
//!
//! - [Header name constants](#constants).
//! - [`Tool::with_mcp_header_param`] builder that records the parameter name
//!   in the tool's `_meta` under [`X_MCP_HEADER_PARAM_META_KEY`]. Load
//!   balancers, tower layers, and custom transport adapters can read the key
//!   off `tools/list` output and promote the parameter at call time.
//! - [`McpHttpHeaders::from_header_map`] utility for extracting the standard
//!   headers from a `http::HeaderMap` (no-op when absent).
//!
//! Gated behind the `sep-2243-draft` cargo feature so upstream sync stays
//! clean while the SEP is still being finalized.

use serde_json::Value;

use crate::model::{Meta, Tool};

// ---------------------------------------------------------------------------
// Header names
// ---------------------------------------------------------------------------

/// HTTP header carrying the JSON-RPC method name of the current request
/// (e.g. `"tools/call"`). SEP-2243 draft.
pub const MCP_METHOD_HEADER: &str = "Mcp-Method";

/// HTTP header carrying the primitive name when the method targets a
/// specific tool / resource / prompt (e.g. the tool name on `tools/call`).
/// SEP-2243 draft.
pub const MCP_NAME_HEADER: &str = "Mcp-Name";

/// HTTP header prefix for tool-parameter pass-through. A tool with a
/// parameter marked via [`Tool::with_mcp_header_param`] will have that
/// parameter's value promoted to a header named
/// `x-mcp-header: <param-name>=<value>` when the transport layer supports
/// the promotion (currently a custom adapter responsibility; rmcp will
/// implement it when SEP-2243 finalizes).
pub const X_MCP_HEADER_NAME: &str = "x-mcp-header";

/// `_meta` key under which [`Tool::with_mcp_header_param`] stores the
/// promotable parameter name. Transport adapters read this off the tool
/// definition and promote the matching parameter value to the
/// [`X_MCP_HEADER_NAME`] header on call.
pub const X_MCP_HEADER_PARAM_META_KEY: &str = "io.modelcontextprotocol/x-mcp-header-param";

// ---------------------------------------------------------------------------
// Tool builder: mark a parameter for header promotion
// ---------------------------------------------------------------------------

impl Tool {
    /// SEP-2243 draft: mark a tool parameter for promotion to an
    /// `x-mcp-header: <param-name>=<value>` HTTP header at call time.
    ///
    /// Stores the parameter name in the tool's `_meta` under
    /// [`X_MCP_HEADER_PARAM_META_KEY`] so transport adapters can discover it
    /// off `tools/list` output. rmcp core does not yet promote the parameter
    /// automatically - when SEP-2243 finalizes, the transport layer will read
    /// this key and perform the promotion.
    ///
    /// Calling this multiple times records multiple parameter names; each
    /// value is merged into a JSON array under the meta key.
    ///
    /// Silently ignored until SEP-2243 lands in Claude Code or the client
    /// implements the pass-through independently.
    #[must_use]
    pub fn with_mcp_header_param(mut self, param_name: impl Into<String>) -> Self {
        let param = Value::from(param_name.into());
        let mut meta = self.meta.unwrap_or_default();
        let existing = meta.0.remove(X_MCP_HEADER_PARAM_META_KEY);
        let next = match existing {
            Some(Value::Array(mut arr)) => {
                if !arr.contains(&param) {
                    arr.push(param);
                }
                Value::Array(arr)
            }
            Some(Value::String(s)) => {
                if param.as_str() == Some(s.as_str()) {
                    Value::String(s)
                } else {
                    Value::Array(vec![Value::String(s), param])
                }
            }
            Some(other) => {
                // Preserve other shapes just in case a downstream uses the key
                // for something we don't expect; convert to an array.
                Value::Array(vec![other, param])
            }
            None => param,
        };
        meta.0.insert(X_MCP_HEADER_PARAM_META_KEY.to_string(), next);
        self.meta = Some(meta);
        self
    }
}

/// Build a [`Meta`] value that carries the SEP-2243 `x-mcp-header-param`
/// marker for a single parameter name. Useful from the `#[tool(meta = ...)]`
/// macro attribute where chaining builder methods on `Tool` is not available.
#[must_use]
pub fn mcp_header_param_meta(param_name: impl Into<String>) -> Meta {
    let mut meta = Meta::new();
    meta.0.insert(
        X_MCP_HEADER_PARAM_META_KEY.to_string(),
        Value::from(param_name.into()),
    );
    meta
}

// ---------------------------------------------------------------------------
// Server-side header extraction
// ---------------------------------------------------------------------------

/// Snapshot of the SEP-2243 standard headers on an inbound HTTP request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[expect(clippy::exhaustive_structs, reason = "intentionally exhaustive")]
pub struct McpHttpHeaders {
    /// Value of `Mcp-Method` (e.g. `"tools/call"`), when present.
    pub method: Option<String>,
    /// Value of `Mcp-Name` (e.g. the tool name), when present.
    pub name: Option<String>,
}

impl McpHttpHeaders {
    /// Extract [`MCP_METHOD_HEADER`] and [`MCP_NAME_HEADER`] from an
    /// `http::HeaderMap`. Returns a default `McpHttpHeaders` when neither
    /// header is present (non-conformant clients will simply not populate
    /// the fields).
    #[cfg(feature = "server-side-http")]
    pub fn from_header_map(headers: &http::HeaderMap) -> Self {
        fn read(headers: &http::HeaderMap, name: &str) -> Option<String> {
            headers
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        }
        Self {
            method: read(headers, MCP_METHOD_HEADER),
            name: read(headers, MCP_NAME_HEADER),
        }
    }

    /// Returns true when the inbound request carried at least one SEP-2243
    /// header (i.e. the client is SEP-2243-aware).
    #[must_use]
    pub fn is_populated(&self) -> bool {
        self.method.is_some() || self.name.is_some()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::model::JsonObject;

    fn sample_tool() -> Tool {
        Tool::new("t", "d", Arc::new(JsonObject::new()))
    }

    #[test]
    fn with_mcp_header_param_stores_single_string() {
        let tool = sample_tool().with_mcp_header_param("trace_id");
        let meta = tool.meta.expect("meta");
        let stored = meta.0.get(X_MCP_HEADER_PARAM_META_KEY).cloned().unwrap();
        assert_eq!(stored, Value::from("trace_id"));
    }

    #[test]
    fn with_mcp_header_param_upgrades_to_array_on_second_call() {
        let tool = sample_tool()
            .with_mcp_header_param("trace_id")
            .with_mcp_header_param("tenant_id");
        let meta = tool.meta.expect("meta");
        let stored = meta.0.get(X_MCP_HEADER_PARAM_META_KEY).cloned().unwrap();
        assert_eq!(
            stored,
            Value::Array(vec![Value::from("trace_id"), Value::from("tenant_id")])
        );
    }

    #[test]
    fn with_mcp_header_param_dedupes_repeats() {
        let tool = sample_tool()
            .with_mcp_header_param("trace_id")
            .with_mcp_header_param("trace_id");
        let meta = tool.meta.expect("meta");
        let stored = meta.0.get(X_MCP_HEADER_PARAM_META_KEY).cloned().unwrap();
        // Second identical call keeps the single-string form.
        assert_eq!(stored, Value::from("trace_id"));
    }

    #[test]
    fn mcp_header_param_meta_builds_standalone_meta() {
        let meta = mcp_header_param_meta("correlation_id");
        assert_eq!(meta.0.len(), 1);
        assert_eq!(
            meta.0.get(X_MCP_HEADER_PARAM_META_KEY).cloned().unwrap(),
            Value::from("correlation_id")
        );
    }

    #[cfg(feature = "server-side-http")]
    #[test]
    fn extract_headers_from_http_header_map() {
        let mut headers = http::HeaderMap::new();
        headers.insert(MCP_METHOD_HEADER, "tools/call".parse().unwrap());
        headers.insert(MCP_NAME_HEADER, "search_assets".parse().unwrap());

        let extracted = McpHttpHeaders::from_header_map(&headers);
        assert!(extracted.is_populated());
        assert_eq!(extracted.method.as_deref(), Some("tools/call"));
        assert_eq!(extracted.name.as_deref(), Some("search_assets"));
    }

    #[cfg(feature = "server-side-http")]
    #[test]
    fn extract_headers_tolerates_absence() {
        let headers = http::HeaderMap::new();
        let extracted = McpHttpHeaders::from_header_map(&headers);
        assert!(!extracted.is_populated());
        assert!(extracted.method.is_none());
        assert!(extracted.name.is_none());
    }
}
