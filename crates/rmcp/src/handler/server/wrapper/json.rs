use std::borrow::Cow;

use schemars::JsonSchema;
use serde::Serialize;

use crate::{
    handler::server::tool::IntoCallToolResult,
    model::{CallToolResult, Content},
};

/// Json wrapper for structured output
///
/// When used with tools, this wrapper indicates that the value should be
/// serialized as structured JSON content with an associated schema.
/// The framework will place the JSON in the `structured_content` field
/// of the tool result rather than the regular `content` field.
#[expect(clippy::exhaustive_structs, reason = "intentionally exhaustive")]
pub struct Json<T>(pub T);

// Implement JsonSchema for Json<T> to delegate to T's schema
impl<T: JsonSchema> JsonSchema for Json<T> {
    fn schema_name() -> Cow<'static, str> {
        T::schema_name()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        T::json_schema(generator)
    }
}

// Implementation for Json<T> to create structured content
impl<T: Serialize + JsonSchema + 'static> IntoCallToolResult for Json<T> {
    fn into_call_tool_result(self) -> Result<CallToolResult, crate::ErrorData> {
        let value = serde_json::to_value(self.0).map_err(|e| {
            crate::ErrorData::internal_error(
                format!("Failed to serialize structured content: {}", e),
                None,
            )
        })?;

        Ok(CallToolResult::structured(value))
    }
}

/// Typed structured response with extra `Content` blocks.
///
/// Returned by tools that emit a `resource_link` / `image` / inline
/// resource alongside their structured response. The serialized JSON of
/// `value` lands at `content[0]` (via `CallToolResult::structured`) and
/// also populates `structuredContent`; `extras` is appended in order.
///
/// The `#[tool]` macro recognizes this wrapper alongside `Json<T>` for
/// `outputSchema` extraction — the schema is taken from the inner `T`.
/// See `extract_json_inner_type` in `rmcp-macros/src/tool.rs`.
///
/// Typical use:
///
/// ```ignore
/// async fn get_log_history(...) -> Result<JsonAndArtifact<LogHistoryResponse>, String> {
///     let resp = self.dispatch_typed::<_, LogHistoryResponse>(&ctx, "get_log_history", &args).await?;
///     artifact_ctx::structured_response("studio-output-log.json", resp.0)
/// }
/// ```
#[expect(clippy::exhaustive_structs, reason = "intentionally exhaustive")]
pub struct JsonAndArtifact<T> {
    pub value: T,
    pub extras: Vec<Content>,
}

// JsonSchema delegates to T so the macro picks up the inner type's schema.
impl<T: JsonSchema> JsonSchema for JsonAndArtifact<T> {
    fn schema_name() -> Cow<'static, str> {
        T::schema_name()
    }

    fn json_schema(generator: &mut schemars::SchemaGenerator) -> schemars::Schema {
        T::json_schema(generator)
    }
}

impl<T: Serialize + JsonSchema + 'static> IntoCallToolResult for JsonAndArtifact<T> {
    fn into_call_tool_result(self) -> Result<CallToolResult, crate::ErrorData> {
        let value = serde_json::to_value(self.value).map_err(|e| {
            crate::ErrorData::internal_error(
                format!("Failed to serialize structured content: {}", e),
                None,
            )
        })?;

        let mut result = CallToolResult::structured(value);
        result.content.extend(self.extras);
        Ok(result)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use schemars::JsonSchema;
    use serde::Serialize;

    use super::*;
    use crate::model::RawResource;

    #[derive(Serialize, JsonSchema)]
    struct DummyResp {
        count: u32,
    }

    /// Empty extras — content[] should carry only the auto-pushed JSON
    /// dump from `CallToolResult::structured`; structured_content set.
    #[test]
    fn json_and_artifact_no_extras_matches_json_shape() {
        let value = DummyResp { count: 7 };
        let result = JsonAndArtifact {
            value,
            extras: vec![],
        }
        .into_call_tool_result()
        .unwrap();

        assert!(result.structured_content.is_some());
        let stored = result.structured_content.as_ref().unwrap();
        assert_eq!(stored, &serde_json::json!({"count": 7}));
        assert_eq!(result.content.len(), 1, "auto-pushed JSON dump only");
        assert_eq!(
            result.content[0].as_text().unwrap().text,
            serde_json::json!({"count": 7}).to_string()
        );
    }

    /// With a resource_link extra: content[0] is the JSON dump,
    /// content[1] is the resource_link; structured_content still set.
    #[test]
    fn json_and_artifact_with_resource_link_appends_to_content() {
        let value = DummyResp { count: 3 };
        let raw = RawResource::new("studio://download/uuid/file.png", "file.png");
        let link = Content::resource_link(raw);
        let result = JsonAndArtifact {
            value,
            extras: vec![link],
        }
        .into_call_tool_result()
        .unwrap();

        assert!(result.structured_content.is_some());
        assert_eq!(result.content.len(), 2, "JSON dump + resource_link");
        assert!(
            result.content[1].raw.as_resource_link().is_some(),
            "second block is a resource_link"
        );
    }

    /// Multiple extras (resource_link + image-style text) preserve order.
    #[test]
    fn json_and_artifact_preserves_extras_order() {
        let value = DummyResp { count: 1 };
        let raw = RawResource::new("studio://download/uuid/a.png", "a.png");
        let extras = vec![Content::resource_link(raw), Content::text("summary")];
        let result = JsonAndArtifact { value, extras }
            .into_call_tool_result()
            .unwrap();

        assert_eq!(result.content.len(), 3);
        assert!(result.content[1].raw.as_resource_link().is_some());
        assert_eq!(result.content[2].as_text().unwrap().text, "summary");
    }

    /// JsonSchema delegates to inner T — a tool returning
    /// `Result<JsonAndArtifact<DummyResp>, _>` advertises the same
    /// outputSchema as `Result<Json<DummyResp>, _>`.
    #[test]
    fn json_and_artifact_schema_delegates_to_inner() {
        assert_eq!(
            <JsonAndArtifact<DummyResp> as JsonSchema>::schema_name(),
            <DummyResp as JsonSchema>::schema_name()
        );
        let mut g1 = schemars::SchemaGenerator::default();
        let mut g2 = schemars::SchemaGenerator::default();
        let s1 = <JsonAndArtifact<DummyResp> as JsonSchema>::json_schema(&mut g1);
        let s2 = <DummyResp as JsonSchema>::json_schema(&mut g2);
        assert_eq!(
            serde_json::to_value(&s1).unwrap(),
            serde_json::to_value(&s2).unwrap()
        );
    }
}
