//! Claude Code-specific MCP extensions.
//!
//! Claude Code (claude-cli) implements two server-facing extensions that aren't in
//! the upstream MCP spec: a per-tool output-size override carried in `_meta`
//! (`anthropic/maxResultSizeChars`, raising the default 25k-token cap up to a
//! 500 000-char ceiling), and an experimental push-message capability
//! (`claude/channel`). This module exposes constants plus a [`Tool`] builder
//! extension so servers can opt into both without hand-rolling JSON keys.
//!
//! All members are passive: they emit what Claude Code expects and are
//! silently ignored by every other MCP client.
//!
//! See <https://docs.anthropic.com/en/docs/claude-code/mcp>.
//!
//! Enable via the `anthropic-ext` cargo feature on `rmcp`.

use serde_json::Value;

use crate::model::{ExtensionCapabilities, JsonObject, Meta, Tool};

/// `_meta` key Claude Code honours on a tool's `tools/list` entry to raise the
/// per-tool text-output size threshold past the default 25 000-token cap.
///
/// See <https://docs.anthropic.com/en/docs/claude-code/mcp#raise-the-limit-for-a-specific-tool>.
pub const MAX_RESULT_SIZE_CHARS_META_KEY: &str = "anthropic/maxResultSizeChars";

/// Hard ceiling Claude Code applies to `anthropic/maxResultSizeChars` values.
/// Tools requesting a larger threshold are clamped to this value.
pub const MAX_RESULT_SIZE_CHARS_CEILING: u32 = 500_000;

/// Experimental capability key Claude Code looks for when started with
/// `--channels`, allowing the server to push messages into the session
/// as out-of-band notifications.
///
/// See <https://docs.anthropic.com/en/docs/claude-code/mcp#push-messages-with-channels>.
pub const CLAUDE_CHANNEL_CAPABILITY: &str = "claude/channel";

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

/// Insert the `claude/channel` extension capability into an
/// [`ExtensionCapabilities`] map with empty settings. Pass the resulting map
/// to [`ServerCapabilitiesBuilder::enable_extensions_with`] when building
/// your server's [`ServerCapabilities`].
///
/// Silently ignored by every MCP client other than Claude Code.
///
/// [`ServerCapabilitiesBuilder::enable_extensions_with`]: crate::model::ServerCapabilities
/// [`ServerCapabilities`]: crate::model::ServerCapabilities
pub fn insert_claude_channel(extensions: &mut ExtensionCapabilities) {
    extensions.insert(CLAUDE_CHANNEL_CAPABILITY.to_string(), JsonObject::new());
}

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
    fn claude_channel_extension_inserted_empty() {
        let mut ext = ExtensionCapabilities::new();
        insert_claude_channel(&mut ext);
        let stored = ext.get(CLAUDE_CHANNEL_CAPABILITY).expect("capability set");
        assert!(stored.is_empty(), "expected empty settings object");
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
}
