//! Claude Code-specific MCP extensions.
//!
//! Claude Code (claude-cli) implements several server-facing extensions that
//! aren't in the upstream MCP spec. This module exposes first-class Rust APIs
//! for each of them so servers can opt in without hand-rolling JSON keys or
//! notification wire formats.
//!
//! - `anthropic/maxResultSizeChars` - per-tool `_meta` entry raising Claude
//!   Code's default 25 000-token output cap up to a 500 000-char ceiling.
//! - `claude/channel` - experimental capability Claude Code honours when
//!   started with `--channels`, allowing the server to push out-of-band
//!   events into the running session.
//! - `claude/channel/permission` - companion capability (Claude Code v2.1.81+)
//!   that opts the channel into remote permission-relay for tool approvals.
//! - `notifications/claude/channel` - the notification method a channel emits
//!   to push events; payload is `{ content, meta? }`.
//! - `structured_with_text_fallback` - MCP 2025-11-25 spec ("a tool that
//!   returns structured content SHOULD also return the serialized JSON in a
//!   TextContent block") + workaround for Claude Code #41361 (blank UI when
//!   `structuredContent` fails `outputSchema.safeParse`). Pushes a
//!   human-readable summary `Content::text` alongside the `Json<T>` structured
//!   value.
//! - `lint::warn_if_over_2kb` - pure `tracing::warn!` helper catching Claude
//!   Code's silent 2 KB truncation of tool descriptions and server
//!   `instructions`.
//!
//! All members are passive: they emit what Claude Code expects and are
//! silently ignored by every other MCP client.
//!
//! See <https://docs.anthropic.com/en/docs/claude-code/mcp> and
//! <https://code.claude.com/docs/en/channels-reference>.
//!
//! Enable via the `anthropic-ext` cargo feature on `rmcp`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::model::{ExperimentalCapabilities, JsonObject, Meta, Tool};
#[cfg(feature = "server")]
use crate::{
    RoleServer,
    handler::server::{tool::IntoCallToolResult, wrapper::Json},
    model::{CallToolResult, Content, CustomNotification, ServerNotification},
    service::{Peer, ServiceError},
};

// ---------------------------------------------------------------------------
// Per-tool output-size override (`anthropic/maxResultSizeChars`)
// ---------------------------------------------------------------------------

/// `_meta` key Claude Code honours on a tool's `tools/list` entry to raise the
/// per-tool text-output size threshold past the default 25 000-token cap.
///
/// See <https://docs.anthropic.com/en/docs/claude-code/mcp#raise-the-limit-for-a-specific-tool>.
pub const MAX_RESULT_SIZE_CHARS_META_KEY: &str = "anthropic/maxResultSizeChars";

/// Hard ceiling Claude Code applies to `anthropic/maxResultSizeChars` values.
/// Tools requesting a larger threshold are clamped to this value.
pub const MAX_RESULT_SIZE_CHARS_CEILING: u32 = 500_000;

impl Tool {
    /// Claude Code-specific: set the `anthropic/maxResultSizeChars` `_meta`
    /// entry on this tool, raising the per-tool text-output threshold in
    /// Claude Code up to a hard ceiling of
    /// [`MAX_RESULT_SIZE_CHARS_CEILING`] (500 000 chars).
    ///
    /// Values above the ceiling are clamped; image output is not affected
    /// (only text `content` entries).
    ///
    /// Silently ignored by every MCP client other than Claude Code.
    /// See <https://docs.anthropic.com/en/docs/claude-code/mcp#raise-the-limit-for-a-specific-tool>.
    #[must_use]
    pub fn with_anthropic_max_result_size_chars(mut self, chars: u32) -> Self {
        let clamped = chars.min(MAX_RESULT_SIZE_CHARS_CEILING);
        let mut meta = self.meta.unwrap_or_default();
        meta.0.insert(
            MAX_RESULT_SIZE_CHARS_META_KEY.to_string(),
            Value::from(clamped),
        );
        self.meta = Some(meta);
        self
    }
}

/// Build a [`Meta`] carrying the `anthropic/maxResultSizeChars` key for direct
/// use in the `#[tool(meta = ...)]` macro attribute. The value is clamped to
/// [`MAX_RESULT_SIZE_CHARS_CEILING`].
///
/// Prefer this helper in macro-driven tool routers where you cannot call
/// [`Tool::with_anthropic_max_result_size_chars`] at construction time. The
/// `_meta` key is silently ignored by every MCP client other than Claude Code.
///
/// See <https://docs.anthropic.com/en/docs/claude-code/mcp#raise-the-limit-for-a-specific-tool>.
///
/// # Example
///
/// ```ignore
/// use rmcp::anthropic_ext::anthropic_max_result_size_chars_meta;
///
/// #[tool(
///     annotations(read_only_hint = true),
///     meta = anthropic_max_result_size_chars_meta(500_000),
/// )]
/// async fn big_output(&self) -> Result<Json<Resp>, String> { /* ... */ }
/// ```
#[must_use]
pub fn anthropic_max_result_size_chars_meta(chars: u32) -> Meta {
    let clamped = chars.min(MAX_RESULT_SIZE_CHARS_CEILING);
    let mut meta = Meta::new();
    meta.0.insert(
        MAX_RESULT_SIZE_CHARS_META_KEY.to_string(),
        Value::from(clamped),
    );
    meta
}

// ---------------------------------------------------------------------------
// Channels (`claude/channel` + `claude/channel/permission`)
// ---------------------------------------------------------------------------

/// Experimental capability key Claude Code looks for when started with
/// `--channels`, allowing the server to push messages into the session
/// as out-of-band notifications.
///
/// See <https://code.claude.com/docs/en/channels-reference>.
pub const CLAUDE_CHANNEL_CAPABILITY: &str = "claude/channel";

/// Experimental capability key Claude Code looks for on channel servers that
/// want tool-approval prompts relayed through the channel (Claude Code
/// v2.1.81+). Declare alongside [`CLAUDE_CHANNEL_CAPABILITY`] to opt in.
///
/// See <https://code.claude.com/docs/en/channels-reference#relay-permission-prompts>.
pub const CLAUDE_CHANNEL_PERMISSION_CAPABILITY: &str = "claude/channel/permission";

/// JSON-RPC method name Claude Code listens on for channel events.
///
/// See <https://code.claude.com/docs/en/channels-reference#notification-format>.
pub const CLAUDE_CHANNEL_NOTIFICATION_METHOD: &str = "notifications/claude/channel";

/// Insert the `claude/channel` capability into an
/// [`ExperimentalCapabilities`] map with empty settings. Pass the resulting
/// map to
/// [`ServerCapabilitiesBuilder::enable_experimental_with`](crate::model::ServerCapabilities)
/// when building your server's [`ServerCapabilities`](crate::model::ServerCapabilities).
///
/// Claude Code reads this key from `capabilities.experimental['claude/channel']`
/// specifically - not the `extensions` field - per the
/// [Claude Code channels reference](https://code.claude.com/docs/en/channels-reference#server-options).
/// Silently ignored by every MCP client other than Claude Code.
pub fn insert_claude_channel(capabilities: &mut ExperimentalCapabilities) {
    capabilities.insert(CLAUDE_CHANNEL_CAPABILITY.to_string(), JsonObject::new());
}

/// Insert the `claude/channel/permission` capability into an
/// [`ExperimentalCapabilities`] map with empty settings. Declare alongside
/// [`insert_claude_channel`] when the channel wants to receive relayed
/// tool-approval prompts. Requires Claude Code v2.1.81+.
///
/// Silently ignored by every MCP client other than Claude Code.
/// See <https://code.claude.com/docs/en/channels-reference#relay-permission-prompts>.
pub fn insert_claude_channel_permission(capabilities: &mut ExperimentalCapabilities) {
    capabilities.insert(
        CLAUDE_CHANNEL_PERMISSION_CAPABILITY.to_string(),
        JsonObject::new(),
    );
}

/// Payload of a [`CLAUDE_CHANNEL_NOTIFICATION_METHOD`] notification.
///
/// `content` becomes the body of the `<channel source="..." ...>` tag injected
/// into Claude's context; each entry in `meta` becomes an attribute on that
/// tag. Meta keys must be identifiers (letters, digits, underscore) - keys
/// containing hyphens or other characters are silently dropped by Claude Code
/// on the receiving side.
///
/// See <https://code.claude.com/docs/en/channels-reference#notification-format>.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[expect(clippy::exhaustive_structs, reason = "intentionally exhaustive")]
pub struct ClaudeChannelNotificationParam {
    /// Body of the `<channel>` tag.
    pub content: String,
    /// Optional meta attributes. Keys must match `[A-Za-z0-9_]+`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meta: Option<BTreeMap<String, String>>,
}

impl ClaudeChannelNotificationParam {
    /// Build a new notification payload with only the required `content`.
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            meta: None,
        }
    }

    /// Attach a single meta attribute. The key must match `[A-Za-z0-9_]+` or
    /// Claude Code will silently drop it.
    #[must_use]
    pub fn with_meta(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.meta
            .get_or_insert_with(BTreeMap::new)
            .insert(key.into(), value.into());
        self
    }

    /// Replace the meta map wholesale.
    #[must_use]
    pub fn with_meta_map(mut self, meta: BTreeMap<String, String>) -> Self {
        self.meta = Some(meta);
        self
    }
}

#[cfg(feature = "server")]
impl Peer<RoleServer> {
    /// Push a `notifications/claude/channel` event into the running Claude
    /// Code session. No-op on every other MCP client (wire-level: the client
    /// sees an unknown notification and drops it).
    ///
    /// See <https://code.claude.com/docs/en/channels-reference#notification-format>.
    pub async fn notify_claude_channel(
        &self,
        params: ClaudeChannelNotificationParam,
    ) -> Result<(), ServiceError> {
        // ClaudeChannelNotificationParam contains only `String` + `BTreeMap<String, String>`
        // fields; serde_json cannot fail for that shape, so an `.expect` is sound.
        let value = serde_json::to_value(&params)
            .expect("ClaudeChannelNotificationParam is always serializable");
        self.send_notification(ServerNotification::CustomNotification(
            CustomNotification::new(CLAUDE_CHANNEL_NOTIFICATION_METHOD, Some(value)),
        ))
        .await
    }
}

// ---------------------------------------------------------------------------
// Structured-content + text-fallback helper
// ---------------------------------------------------------------------------

/// Build a [`CallToolResult`] carrying both structured JSON (`structured_content`)
/// and a **human-readable** text summary (`content`). Addresses two concerns at
/// once:
///
/// - **MCP 2025-11-25 spec:** `/server/tools` says "a tool that returns
///   structured content SHOULD also return the serialized JSON in a
///   TextContent block". [`Json<T>::into_call_tool_result`] already emits the
///   serialized JSON as text. This helper appends a second text block that is
///   human-readable rather than a raw JSON dump.
/// - **Claude Code #41361:** if `structuredContent` fails the client-side
///   `outputSchema.safeParse`, Claude Code renders the tool call **blank** in
///   the UI even though the model still receives the structured value. The
///   extra human-readable text block stays visible to both.
///
/// `text_summary` receives a borrow of the value so callers can render fields
/// without cloning.
///
/// # Example
///
/// ```ignore
/// use rmcp::anthropic_ext::structured_with_text_fallback;
///
/// let resp = ListPlacesResponse { places, next_cursor, truncated: false };
/// return structured_with_text_fallback(resp, |r| {
///     format!("{} places{}", r.places.len(), if r.truncated { " (truncated)" } else { "" })
/// })
/// .map_err(|e| e.to_string());
/// ```
#[cfg(feature = "server")]
pub fn structured_with_text_fallback<T>(
    value: T,
    text_summary: impl FnOnce(&T) -> String,
) -> Result<CallToolResult, crate::ErrorData>
where
    T: serde::Serialize + schemars::JsonSchema + 'static,
{
    let summary = text_summary(&value);
    let mut result = Json(value).into_call_tool_result()?;
    result.content.push(Content::text(summary));
    Ok(result)
}

// ---------------------------------------------------------------------------
// Lint helpers
// ---------------------------------------------------------------------------

/// Lint helpers for catching common Claude Code integration pitfalls at
/// runtime. All helpers are pure - they `tracing::warn!` when a condition is
/// violated but never change behavior.
pub mod lint {
    /// Claude Code truncates tool descriptions and server `instructions` at
    /// 2 048 bytes each, silently and mid-sentence when under aggregate
    /// pressure across multiple servers. See
    /// <https://github.com/anthropics/claude-code/issues/43474>.
    pub const SIZE_LIMIT_BYTES: usize = 2 * 1024;

    /// Emits a `tracing::warn!` when `body.len()` exceeds Claude Code's
    /// 2 KB truncation cliff. Returns `true` if a warning was emitted.
    ///
    /// `label` should identify what was over the limit (e.g. the tool name or
    /// `"instructions.md"`).
    pub fn warn_if_over_2kb(label: &str, body: &str) -> bool {
        let size = body.len();
        if size > SIZE_LIMIT_BYTES {
            tracing::warn!(
                target: "rmcp::anthropic_ext::lint",
                label = label,
                size = size,
                limit = SIZE_LIMIT_BYTES,
                "Claude Code will silently truncate this content at the 2 KB cliff; front-load critical content (see claude-code#43474 for aggregate-truncation caveat)."
            );
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn sample_tool() -> Tool {
        Tool::new("t", "d", Arc::new(JsonObject::new()))
    }

    #[test]
    fn max_result_size_chars_is_clamped() {
        let tool = sample_tool().with_anthropic_max_result_size_chars(1_000_000);
        let meta = tool.meta.expect("meta set");
        let stored = meta
            .0
            .get(MAX_RESULT_SIZE_CHARS_META_KEY)
            .expect("key present");
        assert_eq!(stored, &Value::from(MAX_RESULT_SIZE_CHARS_CEILING));
    }

    #[test]
    fn max_result_size_chars_under_ceiling_kept_verbatim() {
        let tool = sample_tool().with_anthropic_max_result_size_chars(100_000);
        let stored = tool
            .meta
            .expect("meta")
            .0
            .get(MAX_RESULT_SIZE_CHARS_META_KEY)
            .cloned()
            .expect("key");
        assert_eq!(stored, Value::from(100_000u32));
    }

    #[test]
    fn claude_channel_capability_inserted_empty() {
        let mut experimental = ExperimentalCapabilities::new();
        insert_claude_channel(&mut experimental);
        let stored = experimental
            .get(CLAUDE_CHANNEL_CAPABILITY)
            .expect("capability set");
        assert!(stored.is_empty(), "expected empty settings object");
    }

    #[test]
    fn claude_channel_capability_lands_on_experimental_builder_field() {
        use crate::model::ServerCapabilities;

        let mut experimental = ExperimentalCapabilities::new();
        insert_claude_channel(&mut experimental);
        let caps = ServerCapabilities::builder()
            .enable_experimental_with(experimental)
            .build();

        let experimental = caps.experimental.expect("experimental populated");
        assert!(
            experimental.contains_key(CLAUDE_CHANNEL_CAPABILITY),
            "channel key must land on `capabilities.experimental`, not `.extensions`"
        );
        assert!(
            caps.extensions.is_none(),
            "extensions field must stay empty when only `experimental` was populated"
        );
    }

    #[test]
    fn claude_channel_permission_coexists_with_channel() {
        let mut experimental = ExperimentalCapabilities::new();
        insert_claude_channel(&mut experimental);
        insert_claude_channel_permission(&mut experimental);
        assert!(experimental.contains_key(CLAUDE_CHANNEL_CAPABILITY));
        assert!(experimental.contains_key(CLAUDE_CHANNEL_PERMISSION_CAPABILITY));
        assert_eq!(experimental.len(), 2);
    }

    #[test]
    fn channel_notification_param_round_trips() {
        let param = ClaudeChannelNotificationParam::new("hello")
            .with_meta("chat_id", "42")
            .with_meta("severity", "high");

        let json = serde_json::to_value(&param).expect("serialize");
        assert_eq!(json["content"], "hello");
        assert_eq!(json["meta"]["chat_id"], "42");
        assert_eq!(json["meta"]["severity"], "high");

        let back: ClaudeChannelNotificationParam =
            serde_json::from_value(json).expect("deserialize");
        assert_eq!(back.content, "hello");
        let meta = back.meta.expect("meta preserved");
        assert_eq!(meta.get("chat_id").map(String::as_str), Some("42"));
        assert_eq!(meta.get("severity").map(String::as_str), Some("high"));
    }

    #[test]
    fn channel_notification_param_omits_meta_when_empty() {
        let param = ClaudeChannelNotificationParam::new("ping");
        let json = serde_json::to_value(&param).expect("serialize");
        assert_eq!(json["content"], "ping");
        assert!(
            json.get("meta").is_none(),
            "meta key should be omitted when None via skip_serializing_if"
        );
    }

    #[test]
    fn anthropic_max_result_size_chars_meta_under_ceiling() {
        let meta = anthropic_max_result_size_chars_meta(100_000);
        let stored = meta
            .0
            .get(MAX_RESULT_SIZE_CHARS_META_KEY)
            .cloned()
            .expect("key present");
        assert_eq!(stored, Value::from(100_000u32));
    }

    #[test]
    fn anthropic_max_result_size_chars_meta_clamps_above_ceiling() {
        let meta = anthropic_max_result_size_chars_meta(1_000_000);
        let stored = meta
            .0
            .get(MAX_RESULT_SIZE_CHARS_META_KEY)
            .cloned()
            .expect("key present");
        assert_eq!(stored, Value::from(MAX_RESULT_SIZE_CHARS_CEILING));
    }

    #[test]
    fn anthropic_max_result_size_chars_meta_is_standalone() {
        let meta = anthropic_max_result_size_chars_meta(100_000);
        assert_eq!(meta.0.len(), 1, "helper should only emit the size key");
    }

    #[test]
    fn warn_if_over_2kb_returns_true_above_limit() {
        let body = "x".repeat(lint::SIZE_LIMIT_BYTES + 1);
        assert!(lint::warn_if_over_2kb("test", &body));
    }

    #[test]
    fn warn_if_over_2kb_returns_false_at_limit() {
        let body = "x".repeat(lint::SIZE_LIMIT_BYTES);
        assert!(!lint::warn_if_over_2kb("test", &body));
    }

    #[cfg(feature = "server")]
    #[test]
    fn structured_with_text_fallback_appends_summary() {
        #[derive(serde::Serialize, schemars::JsonSchema)]
        struct Resp {
            count: u32,
        }

        let result =
            structured_with_text_fallback(Resp { count: 3 }, |r| format!("{} items", r.count))
                .expect("structured result");

        assert!(
            result.structured_content.is_some(),
            "structured content set"
        );
        // Json<T>::into_call_tool_result pushes one text entry (the serialized
        // JSON); we append a second human-readable summary.
        assert_eq!(result.content.len(), 2, "serialized JSON + summary");

        let last_text = result
            .content
            .last()
            .expect("content non-empty")
            .as_text()
            .expect("final entry is text");
        assert_eq!(last_text.text, "3 items");
    }
}
