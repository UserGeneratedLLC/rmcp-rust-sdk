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
//! - `notifications/claude/channel/permission_request` - inbound notification
//!   Claude Code sends when a tool-approval dialog opens on a channel that
//!   declared `claude/channel/permission`; payload is `{ request_id,
//!   tool_name, description, input_preview }`. Servers parse via
//!   [`CustomNotification::params_as`](crate::model::CustomNotification::params_as)
//!   inside their `ServerHandler::on_custom_notification` override.
//! - `notifications/claude/channel/permission` - outbound verdict the server
//!   emits back; payload is `{ request_id, behavior: "allow" | "deny" }`.
//!   Emit via [`Peer::notify_claude_channel_permission`].
//! - `structured_with_text_fallback` - MCP 2025-11-25 spec ("a tool that
//!   returns structured content SHOULD also return the serialized JSON in a
//!   TextContent block") + workaround for Claude Code #41361 (blank UI when
//!   `structuredContent` fails `outputSchema.safeParse`). Pushes a
//!   human-readable summary `Content::text` alongside the `Json<T>` structured
//!   value.
//! - `normalize_call_tool_result` - silently strips `structuredContent` when
//!   it serializes to an empty object (`{}`). Wired into `ToolRouter::call`
//!   so every tool routed through `#[tool_router]` is normalized
//!   automatically. Claude Code's empty-`{}` shadow bug — and *only* the
//!   empty-`{}` case — drops every block in `content[]`; populated
//!   `structuredContent` coexists fine with `content[]` extras
//!   (`resource_link` / `image` / summary text). Empirically verified
//!   against Claude Code; spec-side, MCP 2025-11-25 has no precedence
//!   rule between `content[]` and `structuredContent` (SEP #2200 still
//!   in flight).
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
// Channel permission relay (`claude/channel/permission`)
// ---------------------------------------------------------------------------

/// JSON-RPC method Claude Code uses to forward a tool-approval prompt into a
/// channel that declared [`CLAUDE_CHANNEL_PERMISSION_CAPABILITY`]. The server
/// receives this as an inbound [`CustomNotification`]; parse via
/// [`CustomNotification::params_as`](crate::model::CustomNotification::params_as)
/// into [`ClaudeChannelPermissionRequestParam`].
///
/// See <https://code.claude.com/docs/en/channels-reference#relay-permission-prompts>.
pub const CLAUDE_CHANNEL_PERMISSION_REQUEST_METHOD: &str =
    "notifications/claude/channel/permission_request";

/// JSON-RPC method the server emits back to Claude Code carrying the verdict
/// for a prior [`CLAUDE_CHANNEL_PERMISSION_REQUEST_METHOD`]. Emit typed via
/// [`Peer::notify_claude_channel_permission`].
///
/// See <https://code.claude.com/docs/en/channels-reference#relay-permission-prompts>.
pub const CLAUDE_CHANNEL_PERMISSION_METHOD: &str = "notifications/claude/channel/permission";

/// Payload of an inbound [`CLAUDE_CHANNEL_PERMISSION_REQUEST_METHOD`]
/// notification. Claude Code sends one of these whenever a local tool-approval
/// dialog opens on a channel that declared
/// [`CLAUDE_CHANNEL_PERMISSION_CAPABILITY`].
///
/// `request_id` is five lowercase letters drawn from `a`-`z` skipping `l`; it
/// must be echoed verbatim in the corresponding
/// [`ClaudeChannelPermissionParam::request_id`] or Claude Code drops the
/// verdict silently. The local terminal dialog never displays this ID.
///
/// See <https://code.claude.com/docs/en/channels-reference#permission-request-fields>.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[expect(clippy::exhaustive_structs, reason = "intentionally exhaustive")]
pub struct ClaudeChannelPermissionRequestParam {
    /// Five-letter ID Claude Code issued for this dialog; echo verbatim.
    pub request_id: String,
    /// Tool Claude wants to invoke (e.g. `"Bash"`, `"Write"`).
    pub tool_name: String,
    /// Human-readable summary of the specific call; same text the local
    /// terminal dialog shows.
    pub description: String,
    /// Tool arguments rendered as a JSON string, truncated by Claude Code to
    /// ~200 characters.
    pub input_preview: String,
}

/// Verdict the server returns in a [`CLAUDE_CHANNEL_PERMISSION_METHOD`]
/// notification. `Allow` proceeds with the tool call; `Deny` rejects it
/// (equivalent to answering "No" in the local dialog). Neither verdict
/// affects future calls — each dialog is independent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
#[expect(clippy::exhaustive_enums, reason = "verdict set is spec-defined")]
pub enum ClaudeChannelPermissionVerdict {
    /// Proceed with the tool call.
    Allow,
    /// Reject the tool call.
    Deny,
}

/// Payload of an outbound [`CLAUDE_CHANNEL_PERMISSION_METHOD`] notification.
/// Pair `request_id` with the value received in the corresponding inbound
/// [`ClaudeChannelPermissionRequestParam::request_id`] exactly — Claude Code
/// only accepts a verdict that carries an ID it issued and is still pending.
///
/// See <https://code.claude.com/docs/en/channels-reference#relay-permission-prompts>.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[expect(clippy::exhaustive_structs, reason = "intentionally exhaustive")]
pub struct ClaudeChannelPermissionParam {
    /// Five-letter ID echoed from the originating
    /// [`ClaudeChannelPermissionRequestParam::request_id`].
    pub request_id: String,
    /// Verdict to apply: `Allow` or `Deny`.
    pub behavior: ClaudeChannelPermissionVerdict,
}

impl ClaudeChannelPermissionParam {
    /// Build a verdict payload from an ID and behavior.
    pub fn new(request_id: impl Into<String>, behavior: ClaudeChannelPermissionVerdict) -> Self {
        Self {
            request_id: request_id.into(),
            behavior,
        }
    }

    /// Shorthand for `new(id, Allow)` — the canonical autonomous-pipeline
    /// auto-approve path.
    #[must_use]
    pub fn allow(request_id: impl Into<String>) -> Self {
        Self::new(request_id, ClaudeChannelPermissionVerdict::Allow)
    }

    /// Shorthand for `new(id, Deny)`.
    #[must_use]
    pub fn deny(request_id: impl Into<String>) -> Self {
        Self::new(request_id, ClaudeChannelPermissionVerdict::Deny)
    }
}

#[cfg(feature = "server")]
impl Peer<RoleServer> {
    /// Push a `notifications/claude/channel/permission` verdict back to Claude
    /// Code. Call this from
    /// [`ServerHandler::on_custom_notification`](crate::ServerHandler::on_custom_notification)
    /// once a [`CLAUDE_CHANNEL_PERMISSION_REQUEST_METHOD`] notification has
    /// been parsed into a [`ClaudeChannelPermissionRequestParam`] and a
    /// verdict has been decided. `request_id` in `params` must match the
    /// originating request or Claude Code drops the verdict silently.
    ///
    /// Autonomous pipelines typically pair this with a sender-gated channel
    /// and always emit `Allow` (no human in the loop to make the call). See
    /// <https://code.claude.com/docs/en/channels-reference#relay-permission-prompts>.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use rmcp::{
    ///     ServerHandler,
    ///     anthropic_ext::{
    ///         CLAUDE_CHANNEL_PERMISSION_REQUEST_METHOD,
    ///         ClaudeChannelPermissionParam,
    ///         ClaudeChannelPermissionRequestParam,
    ///     },
    ///     model::CustomNotification,
    ///     service::{NotificationContext, RoleServer},
    /// };
    ///
    /// impl ServerHandler for MyServer {
    ///     async fn on_custom_notification(
    ///         &self,
    ///         notification: CustomNotification,
    ///         context: NotificationContext<RoleServer>,
    ///     ) {
    ///         if notification.method != CLAUDE_CHANNEL_PERMISSION_REQUEST_METHOD {
    ///             return;
    ///         }
    ///         let Ok(Some(req)) =
    ///             notification.params_as::<ClaudeChannelPermissionRequestParam>()
    ///         else {
    ///             return;
    ///         };
    ///         let _ = context
    ///             .peer
    ///             .notify_claude_channel_permission(
    ///                 ClaudeChannelPermissionParam::allow(req.request_id),
    ///             )
    ///             .await;
    ///     }
    /// }
    /// ```
    pub async fn notify_claude_channel_permission(
        &self,
        params: ClaudeChannelPermissionParam,
    ) -> Result<(), ServiceError> {
        // ClaudeChannelPermissionParam contains only a `String` + a unit-like
        // enum, so `serde_json::to_value` cannot fail for that shape.
        let value = serde_json::to_value(&params)
            .expect("ClaudeChannelPermissionParam is always serializable");
        self.send_notification(ServerNotification::CustomNotification(
            CustomNotification::new(CLAUDE_CHANNEL_PERMISSION_METHOD, Some(value)),
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
/// # Empty-structured normalization
///
/// When `value` serializes to an empty object `{}` (e.g. a response struct
/// whose only fields are `Option<...>` skipped via `skip_serializing_if`),
/// the resulting `structuredContent: {}` would shadow every extra block
/// pushed onto `content[]` afterward (`resource_link`, `image`, summary text)
/// on Claude Code — see [`normalize_call_tool_result`]. The normalizer is
/// wired into `ToolRouter::call` so every tool routed through
/// `#[tool_router]` is stripped automatically; you do not need to hand-strip
/// empties at call sites.
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
// CallToolResult normalization (`normalize_call_tool_result`)
// ---------------------------------------------------------------------------

/// Silently strip `structuredContent` when it serializes to an empty object
/// `{}`.
///
/// Claude Code's empty-`{}` shadow bug: when `structuredContent` is
/// `Some(Value::Object({}))`, Claude Code surfaces only that empty
/// envelope to the model and drops every block in `content[]`
/// (`resource_link`, `image`, human-readable summary text). Stripping
/// the redundant `{}` envelope server-side unblocks the model seeing the
/// extras. The serialized JSON dump auto-pushed onto `content[0]` by
/// [`Json<T>::into_call_tool_result`] already carries the same payload
/// (which is `null` / `{}` here), so this normalization is
/// information-preserving.
///
/// Populated `structuredContent` does NOT shadow `content[]` — both
/// surface to the model side-by-side. Empirically verified against
/// Claude Code; the MCP 2025-11-25 spec has no precedence rule between
/// the two fields (SEP #2200 still in flight). The empty-`{}` case is
/// the only known shadow trigger and the only thing this normalizer
/// addresses.
///
/// Covers tools whose response struct serializes to `{}` — full-viewport
/// screenshots, artifact-only downloads with `inline=false`, count-only
/// thumbnail tools, and any tool that conditionally omits every field
/// (`Option<...>` with `skip_serializing_if`). Tools with a populated
/// structured response and `content[]` extras (e.g. cropped screenshots
/// emitting a populated `crop_bounds` alongside a `resource_link`) need
/// no special handling — both fields reach the model.
///
/// Wired into [`ToolRouter::call`](crate::handler::server::tool::ToolRouter)
/// so every tool routed through `#[tool_router]` is normalized
/// automatically. Tool authors do not call this directly.
///
/// See <https://github.com/anthropics/claude-code/issues/41361> for the
/// adjacent `outputSchema.safeParse` blank-UI bug; that's a different
/// failure mode, not addressed by this normalizer.
#[cfg(feature = "server")]
pub fn normalize_call_tool_result(result: &mut CallToolResult) {
    let is_empty_object = matches!(
        result.structured_content.as_ref(),
        Some(Value::Object(map)) if map.is_empty()
    );
    if is_empty_object {
        result.structured_content = None;
    }
}

// ---------------------------------------------------------------------------
// Test support — public helpers for unit-testing custom `ServerHandler` impls
// ---------------------------------------------------------------------------

/// Public test helpers for unit-testing custom [`ServerHandler`](crate::ServerHandler)
/// impls — in particular hand-rolled `call_tool` overrides — from outside the
/// `rmcp` crate.
///
/// Workspace MCPs that wrap their `tool_router` in
/// `Arc<RwLock<ToolRouter<Self>>>` (to support runtime mutation, e.g.
/// `load_tool_group`) cannot use rmcp's `#[tool_handler]` macro and must
/// hand-roll [`ServerHandler::call_tool`](crate::ServerHandler::call_tool).
/// That dispatch path bypasses [`ToolRouter::call`](crate::handler::server::router::tool::ToolRouter)
/// and therefore the post-dispatch [`normalize_call_tool_result`] hook —
/// each consumer must call the normalizer manually inside its hand-rolled
/// `call_tool`. The helpers in this module make it possible to drive that
/// custom dispatch path from a unit test without spinning up a full
/// `serve_server` / transport stack.
///
/// All helpers are gated on the `server` feature; the throwaway peer's
/// outbound channel is dropped immediately, so any peer-bound notifications
/// the tool body emits silently no-op.
#[cfg(feature = "server")]
pub mod test_support {
    use std::sync::Arc;

    use crate::{
        RoleServer,
        model::NumberOrString,
        service::{AtomicU32RequestIdProvider, Peer, RequestContext, RequestIdProvider},
    };

    /// Build a [`RequestContext<RoleServer>`] suitable for unit-testing
    /// [`ServerHandler::call_tool`](crate::ServerHandler::call_tool) (and
    /// other server methods that take a request context).
    ///
    /// Internally constructs a throwaway [`Peer<RoleServer>`] via the
    /// crate-internal constructor; the receiving channel is dropped, so any
    /// peer-bound notifications the tool body emits silently no-op. The
    /// request id is fixed to `1`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// use rmcp::{
    ///     ServerHandler,
    ///     anthropic_ext::test_support::request_context_for_test,
    ///     model::CallToolRequestParams,
    /// };
    ///
    /// # async fn smoke<S: ServerHandler>(server: &S) {
    /// let req: CallToolRequestParams =
    ///     serde_json::from_value(serde_json::json!({ "name": "ping" })).unwrap();
    /// let ctx = request_context_for_test();
    /// let result = server.call_tool(req, ctx).await.expect("call_tool");
    /// // ... assertions ...
    /// # }
    /// ```
    #[must_use]
    pub fn request_context_for_test() -> RequestContext<RoleServer> {
        let id_provider: Arc<dyn RequestIdProvider> =
            Arc::new(AtomicU32RequestIdProvider::default());
        let (peer, _rx) = Peer::<RoleServer>::new(id_provider, None);
        RequestContext::new(NumberOrString::Number(1), peer)
    }
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

    #[cfg(feature = "server")]
    #[test]
    fn normalize_strips_empty_object() {
        use crate::model::CallToolResult;

        let mut result = CallToolResult::structured(serde_json::json!({}));
        assert!(
            result.structured_content.is_some(),
            "test setup: structured starts populated"
        );
        normalize_call_tool_result(&mut result);
        assert!(
            result.structured_content.is_none(),
            "empty `{{}}` envelope must be stripped so Claude Code surfaces content[]"
        );
    }

    #[cfg(feature = "server")]
    #[test]
    fn normalize_keeps_populated_structured() {
        use crate::model::CallToolResult;

        let mut result = CallToolResult::structured(serde_json::json!({"k": "v"}));
        normalize_call_tool_result(&mut result);
        let stored = result
            .structured_content
            .as_ref()
            .expect("populated structured must survive normalization");
        assert_eq!(stored, &serde_json::json!({"k": "v"}));
    }

    #[cfg(feature = "server")]
    #[test]
    fn normalize_keeps_populated_structured_alongside_extras() {
        use crate::model::{CallToolResult, Content, RawResource};

        let mut result = CallToolResult::structured(serde_json::json!({"k": "v"}));
        let raw = RawResource::new("studio://download/uuid/file.png", "file.png");
        result.content.push(Content::resource_link(raw));

        normalize_call_tool_result(&mut result);

        // Intended behavior: populated structured + extras coexist fine.
        // Empirically verified against Claude Code — only the empty-{}
        // case shadows content[]. Tools that emit a resource_link
        // alongside a populated typed body need no special handling.
        assert!(
            result.structured_content.is_some(),
            "populated structuredContent survives alongside content[] extras"
        );
    }

    #[cfg(feature = "server")]
    #[test]
    fn normalize_leaves_none_structured_alone() {
        use crate::model::{CallToolResult, Content};

        let mut result = CallToolResult::success(vec![Content::text("hi")]);
        assert!(result.structured_content.is_none());
        normalize_call_tool_result(&mut result);
        assert!(
            result.structured_content.is_none(),
            "None structured stays None"
        );
    }

    #[cfg(feature = "server")]
    #[test]
    fn normalize_leaves_non_object_structured_alone() {
        use crate::model::CallToolResult;

        // Json<T>::into_call_tool_result always emits an Object, but
        // CallToolResult::structured(value) accepts arbitrary JSON. Defensive
        // check that a non-object Value (array, string, etc.) doesn't get
        // matched as "empty" by the normalizer.
        let mut result = CallToolResult::structured(serde_json::json!([]));
        normalize_call_tool_result(&mut result);
        assert!(
            result.structured_content.is_some(),
            "non-object structured (array, scalar, etc.) is left untouched"
        );
    }

    #[cfg(feature = "server")]
    #[test]
    fn test_support_request_context_constructs() {
        // Sanity check: the public test helper produces a usable
        // RequestContext<RoleServer> that consumer crates can pass into a
        // hand-rolled ServerHandler::call_tool from unit tests.
        let ctx = test_support::request_context_for_test();
        match ctx.id {
            crate::model::NumberOrString::Number(n) => assert_eq!(n, 1),
            crate::model::NumberOrString::String(_) => {
                panic!("request_context_for_test() should produce a numeric id")
            }
        }
    }

    // ----- permission relay -------------------------------------------------

    #[test]
    fn permission_method_constants_match_spec() {
        assert_eq!(
            CLAUDE_CHANNEL_PERMISSION_REQUEST_METHOD,
            "notifications/claude/channel/permission_request"
        );
        assert_eq!(
            CLAUDE_CHANNEL_PERMISSION_METHOD,
            "notifications/claude/channel/permission"
        );
    }

    #[test]
    fn permission_request_param_deserializes_from_claude_code_payload() {
        let raw = serde_json::json!({
            "request_id": "abcde",
            "tool_name": "Bash",
            "description": "list the files in this directory",
            "input_preview": "{\"command\": \"ls\"}"
        });
        let parsed: ClaudeChannelPermissionRequestParam =
            serde_json::from_value(raw).expect("deserialize");
        assert_eq!(parsed.request_id, "abcde");
        assert_eq!(parsed.tool_name, "Bash");
        assert_eq!(parsed.description, "list the files in this directory");
        assert_eq!(parsed.input_preview, "{\"command\": \"ls\"}");
    }

    #[test]
    fn permission_verdict_serializes_snake_case() {
        let allow = serde_json::to_value(ClaudeChannelPermissionVerdict::Allow).expect("serialize");
        let deny = serde_json::to_value(ClaudeChannelPermissionVerdict::Deny).expect("serialize");
        assert_eq!(allow, serde_json::Value::String("allow".into()));
        assert_eq!(deny, serde_json::Value::String("deny".into()));
    }

    #[test]
    fn permission_verdict_round_trips() {
        for v in [
            ClaudeChannelPermissionVerdict::Allow,
            ClaudeChannelPermissionVerdict::Deny,
        ] {
            let json = serde_json::to_value(v).expect("serialize");
            let back: ClaudeChannelPermissionVerdict =
                serde_json::from_value(json).expect("deserialize");
            assert_eq!(back, v);
        }
    }

    #[test]
    fn permission_param_wire_shape_matches_spec() {
        let param = ClaudeChannelPermissionParam::allow("abcde");
        let json = serde_json::to_value(&param).expect("serialize");
        assert_eq!(json["request_id"], "abcde");
        assert_eq!(json["behavior"], "allow");
        assert_eq!(
            json.as_object().map(serde_json::Map::len),
            Some(2),
            "payload carries exactly `request_id` and `behavior`"
        );
    }

    #[test]
    fn permission_param_allow_and_deny_constructors() {
        let allow = ClaudeChannelPermissionParam::allow("fghij");
        assert_eq!(allow.request_id, "fghij");
        assert_eq!(allow.behavior, ClaudeChannelPermissionVerdict::Allow);

        let deny = ClaudeChannelPermissionParam::deny("klmno");
        assert_eq!(deny.request_id, "klmno");
        assert_eq!(deny.behavior, ClaudeChannelPermissionVerdict::Deny);
    }

    #[test]
    fn permission_request_parseable_via_custom_notification_params_as() {
        // Mirrors the consumer path: inbound CustomNotification -> params_as::<T>().
        use crate::model::CustomNotification;

        let payload = serde_json::json!({
            "request_id": "pqrst",
            "tool_name": "Write",
            "description": "Create new file README.md",
            "input_preview": "{\"path\":\"./README.md\",\"content\":\"# Proj...\"}"
        });
        let notification = CustomNotification::new(
            CLAUDE_CHANNEL_PERMISSION_REQUEST_METHOD,
            Some(payload.clone()),
        );
        assert_eq!(
            notification.method,
            CLAUDE_CHANNEL_PERMISSION_REQUEST_METHOD
        );

        let parsed: ClaudeChannelPermissionRequestParam = notification
            .params_as()
            .expect("params_as succeeds")
            .expect("params were Some");
        assert_eq!(parsed.request_id, "pqrst");
        assert_eq!(parsed.tool_name, "Write");
    }
}
