//! Common utilities shared between tool and prompt handlers

use std::{any::TypeId, collections::HashMap, sync::Arc};

use schemars::JsonSchema;

use crate::{
    RoleServer, model::JsonObject, schemars::generate::SchemaSettings, service::RequestContext,
};

/// Generates a JSON schema for a type
pub fn schema_for_type<T: JsonSchema + std::any::Any>() -> Arc<JsonObject> {
    thread_local! {
        static CACHE_FOR_TYPE: std::sync::RwLock<HashMap<TypeId, Arc<JsonObject>>> = Default::default();
    };
    CACHE_FOR_TYPE.with(|cache| {
        if let Some(x) = cache
            .read()
            .expect("schema cache lock poisoned")
            .get(&TypeId::of::<T>())
        {
            x.clone()
        } else {
            // explicitly to align json schema version to official specifications.
            // refer to https://github.com/modelcontextprotocol/modelcontextprotocol/pull/655 for details.
            let settings = SchemaSettings::draft2020_12();
            // Note: AddNullable is intentionally NOT used here because the `nullable` keyword
            // is an OpenAPI 3.0 extension, not part of JSON Schema 2020-12. Using it would
            // cause validation failures with strict JSON Schema validators.
            let generator = settings.into_generator();
            let mut schema = generator.into_root_schema_for::<T>();

            // Claude Code's `LocalMcpServerManager` schema walker throws a
            // `TypeError` (`Cannot use 'in' operator to search for 'properties' in
            // true`) when traversing a tool schema and meeting a bare boolean
            // subschema (`serde_json::Value` -> `true`, `Vec<serde_json::Value>` ->
            // `items: true`, `BTreeMap<_, Value>` -> `additionalProperties: true`,
            // `#[serde(deny_unknown_fields)]` -> `additionalProperties: false`).
            // The crash takes out the entire server's tool list for the session.
            // JSON Schema 2020-12 permits booleans as subschema shortcuts (`true`
            // = `{}`, `false` = `{"not": {}}`) and the MCP spec mandates 2020-12
            // (SEP-1613), so this is a client compliance defect, not a server bug.
            // Normalise to object form before serialisation. See
            // anthropics/claude-code#50194, #25081.
            //
            // Applied via `transform_subschemas` (not `SchemaSettings::with_transform`)
            // so a degenerate `Json<serde_json::Value>` root stays a bare boolean
            // and the existing non-object-root panic below still fires.
            let mut replace = schemars::transform::ReplaceBoolSchemas::default();
            schemars::transform::transform_subschemas(&mut replace, &mut schema);

            let object = serde_json::to_value(schema).expect("failed to serialize schema");
            let object = match object {
                serde_json::Value::Object(object) => object,
                _ => panic!(
                    "Schema serialization produced non-object value: expected JSON object but got {:?}",
                    object
                ),
            };
            let schema = Arc::new(object);
            cache
                .write()
                .expect("schema cache lock poisoned")
                .insert(TypeId::of::<T>(), schema.clone());

            schema
        }
    })
}

// TODO: should be updated according to the new specifications
/// Schema used when input is empty.
pub fn schema_for_empty_input() -> Arc<JsonObject> {
    std::sync::Arc::new(
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
        .as_object()
        .unwrap()
        .clone(),
    )
}

/// Generate and validate a JSON schema for outputSchema (must have root type "object").
pub fn schema_for_output<T: JsonSchema + std::any::Any>() -> Result<Arc<JsonObject>, String> {
    thread_local! {
        static CACHE_FOR_OUTPUT: std::sync::RwLock<HashMap<TypeId, Result<Arc<JsonObject>, String>>> = Default::default();
    };

    CACHE_FOR_OUTPUT.with(|cache| {
        // Try to get from cache first
        if let Some(result) = cache
            .read()
            .expect("output schema cache lock poisoned")
            .get(&TypeId::of::<T>())
        {
            return result.clone();
        }

        // Generate and validate schema
        let schema = schema_for_type::<T>();
        let result = match schema.get("type") {
            Some(serde_json::Value::String(t)) if t == "object" => Ok(schema.clone()),
            Some(serde_json::Value::String(t)) => Err(format!(
                "MCP specification requires tool outputSchema to have root type 'object', but found '{}'.",
                t
            )),
            None => Err(
                "Schema is missing 'type' field. MCP specification requires outputSchema to have root type 'object'.".to_string()
            ),
            Some(other) => Err(format!(
                "Schema 'type' field has unexpected format: {:?}. Expected \"object\".",
                other
            )),
        };

        // Cache the result (both success and error cases)
        cache
            .write()
            .expect("output schema cache lock poisoned")
            .insert(TypeId::of::<T>(), result.clone());

        result
    })
}

/// Trait for extracting parts from a context, unifying tool and prompt extraction
pub trait FromContextPart<C>: Sized {
    fn from_context_part(context: &mut C) -> Result<Self, crate::ErrorData>;
}

/// Common extractors that can be used by both tool and prompt handlers
impl<C> FromContextPart<C> for RequestContext<RoleServer>
where
    C: AsRequestContext,
{
    fn from_context_part(context: &mut C) -> Result<Self, crate::ErrorData> {
        Ok(context.as_request_context().clone())
    }
}

impl<C> FromContextPart<C> for tokio_util::sync::CancellationToken
where
    C: AsRequestContext,
{
    fn from_context_part(context: &mut C) -> Result<Self, crate::ErrorData> {
        Ok(context.as_request_context().ct.clone())
    }
}

impl<C> FromContextPart<C> for crate::model::Extensions
where
    C: AsRequestContext,
{
    fn from_context_part(context: &mut C) -> Result<Self, crate::ErrorData> {
        Ok(context.as_request_context().extensions.clone())
    }
}

#[expect(clippy::exhaustive_structs, reason = "intentionally exhaustive")]
pub struct Extension<T>(pub T);

impl<C, T> FromContextPart<C> for Extension<T>
where
    C: AsRequestContext,
    T: Send + Sync + 'static + Clone,
{
    fn from_context_part(context: &mut C) -> Result<Self, crate::ErrorData> {
        let extension = context
            .as_request_context()
            .extensions
            .get::<T>()
            .cloned()
            .ok_or_else(|| {
                crate::ErrorData::invalid_params(
                    format!("missing extension {}", std::any::type_name::<T>()),
                    None,
                )
            })?;
        Ok(Extension(extension))
    }
}

impl<C> FromContextPart<C> for crate::Peer<RoleServer>
where
    C: AsRequestContext,
{
    fn from_context_part(context: &mut C) -> Result<Self, crate::ErrorData> {
        Ok(context.as_request_context().peer.clone())
    }
}

impl<C> FromContextPart<C> for crate::model::Meta
where
    C: AsRequestContext,
{
    fn from_context_part(context: &mut C) -> Result<Self, crate::ErrorData> {
        let request_context = context.as_request_context_mut();
        let mut meta = crate::model::Meta::default();
        std::mem::swap(&mut meta, &mut request_context.meta);
        Ok(meta)
    }
}

#[expect(clippy::exhaustive_structs, reason = "intentionally exhaustive")]
pub struct RequestId(pub crate::model::RequestId);

impl<C> FromContextPart<C> for RequestId
where
    C: AsRequestContext,
{
    fn from_context_part(context: &mut C) -> Result<Self, crate::ErrorData> {
        Ok(RequestId(context.as_request_context().id.clone()))
    }
}

/// Trait for types that can provide access to RequestContext
pub trait AsRequestContext {
    fn as_request_context(&self) -> &RequestContext<RoleServer>;
    fn as_request_context_mut(&mut self) -> &mut RequestContext<RoleServer>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(serde::Serialize, serde::Deserialize, JsonSchema)]
    struct TestObject {
        value: i32,
    }

    #[derive(serde::Serialize, serde::Deserialize, JsonSchema)]
    struct AnotherTestObject {
        value: i32,
    }

    #[test]
    fn test_schema_for_type_handles_primitive() {
        let schema = schema_for_type::<i32>();

        assert_eq!(schema.get("type"), Some(&serde_json::json!("integer")));
    }

    #[test]
    fn test_schema_for_type_handles_array() {
        let schema = schema_for_type::<Vec<i32>>();

        assert_eq!(schema.get("type"), Some(&serde_json::json!("array")));
        let items = schema.get("items").and_then(|v| v.as_object());
        assert_eq!(
            items.unwrap().get("type"),
            Some(&serde_json::json!("integer"))
        );
    }

    #[test]
    fn test_schema_for_type_handles_struct() {
        let schema = schema_for_type::<TestObject>();

        assert_eq!(schema.get("type"), Some(&serde_json::json!("object")));
        let properties = schema.get("properties").and_then(|v| v.as_object());
        assert!(properties.unwrap().contains_key("value"));
    }

    #[test]
    fn test_schema_for_type_caches_primitive_types() {
        let schema1 = schema_for_type::<i32>();
        let schema2 = schema_for_type::<i32>();

        assert!(Arc::ptr_eq(&schema1, &schema2));
    }

    #[test]
    fn test_schema_for_type_caches_struct_types() {
        let schema1 = schema_for_type::<TestObject>();
        let schema2 = schema_for_type::<TestObject>();

        assert!(Arc::ptr_eq(&schema1, &schema2));
    }

    #[test]
    fn test_schema_for_type_different_types_different_schemas() {
        let schema1 = schema_for_type::<TestObject>();
        let schema2 = schema_for_type::<AnotherTestObject>();

        assert!(!Arc::ptr_eq(&schema1, &schema2));
    }

    #[test]
    fn test_schema_for_type_arc_can_be_shared() {
        let schema = schema_for_type::<TestObject>();
        let cloned = schema.clone();

        assert!(Arc::ptr_eq(&schema, &cloned));
    }

    #[test]
    fn test_schema_for_output_rejects_primitive() {
        let result = schema_for_output::<i32>();
        assert!(result.is_err(),);
    }

    #[test]
    fn test_schema_for_output_accepts_object() {
        let result = schema_for_output::<TestObject>();
        assert!(result.is_ok(),);
    }

    // -------------------------------------------------------------------
    // Boolean-subschema normalisation tests
    //
    // `serde_json::Value`'s `JsonSchema` impl returns `true.into()` —
    // bare boolean schemas. JSON Schema 2020-12 permits these
    // (§4.3.2: `true` ≡ `{}`, `false` ≡ `{"not": {}}`), but Claude Code's
    // `LocalMcpServerManager` schema walker throws a TypeError on
    // contact (anthropics/claude-code#50194), silently dropping every
    // tool from the affected server. `schema_for_type` therefore runs
    // `schemars::transform::ReplaceBoolSchemas` over all subschemas
    // before serialisation. These tests pin that contract.
    // -------------------------------------------------------------------

    /// Walk a JSON value tree and return paths of any bare `Value::Bool`
    /// nodes — used by the bool-subschema tests below to assert the
    /// post-transform schema is fully boolean-free.
    fn find_bool_subschemas(node: &serde_json::Value, path: &str) -> Vec<String> {
        const OBJECT_OF_SCHEMAS: &[&str] =
            &["properties", "patternProperties", "$defs", "definitions"];
        const ARRAY_OF_SCHEMAS: &[&str] = &["oneOf", "anyOf", "allOf", "prefixItems"];
        const SINGLE_SCHEMA: &[&str] = &[
            "items",
            "additionalProperties",
            "additionalItems",
            "not",
            "if",
            "then",
            "else",
            "contains",
            "propertyNames",
        ];
        let mut offenders = Vec::new();
        let serde_json::Value::Object(obj) = node else {
            return offenders;
        };
        for (key, value) in obj {
            let k = key.as_str();
            if OBJECT_OF_SCHEMAS.contains(&k) {
                if let Some(map) = value.as_object() {
                    for (k2, v2) in map {
                        let p = format!("{path}.{k}.{k2}");
                        if v2.is_boolean() {
                            offenders.push(p);
                        } else {
                            offenders.extend(find_bool_subschemas(v2, &format!("{path}.{k}.{k2}")));
                        }
                    }
                }
            } else if ARRAY_OF_SCHEMAS.contains(&k) {
                if let Some(arr) = value.as_array() {
                    for (i, v2) in arr.iter().enumerate() {
                        let p = format!("{path}.{k}[{i}]");
                        if v2.is_boolean() {
                            offenders.push(p);
                        } else {
                            offenders.extend(find_bool_subschemas(v2, &p));
                        }
                    }
                }
            } else if SINGLE_SCHEMA.contains(&k) {
                let p = format!("{path}.{k}");
                if value.is_boolean() {
                    offenders.push(p);
                } else {
                    offenders.extend(find_bool_subschemas(value, &p));
                }
            }
        }
        offenders
    }

    #[derive(serde::Serialize, serde::Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct ValueFieldHolder {
        v: serde_json::Value,
    }

    /// `serde_json::Value` field → `properties.v` is `{}`, not `true`.
    /// This is the canonical `Json<DataStoreEntry>`-style case from
    /// roblox-mcp.
    #[test]
    fn value_field_normalised_to_empty_object() {
        let schema = schema_for_type::<ValueFieldHolder>();
        let properties = schema
            .get("properties")
            .and_then(|v| v.as_object())
            .expect("properties");
        let v_schema = properties.get("v").expect("properties.v");
        assert_eq!(
            v_schema,
            &serde_json::json!({}),
            "properties.v must be normalised to `{{}}`, was {v_schema}"
        );
    }

    #[derive(serde::Serialize, serde::Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct VecValueHolder {
        xs: Vec<serde_json::Value>,
    }

    /// `Vec<serde_json::Value>` field → `properties.xs.items` is `{}`,
    /// not `true`. Canonical `GetThumbnailAnalyticsResponse`-style case.
    #[test]
    fn vec_value_items_normalised() {
        let schema = schema_for_type::<VecValueHolder>();
        let xs_schema = schema
            .get("properties")
            .and_then(|v| v.as_object())
            .and_then(|p| p.get("xs"))
            .and_then(|v| v.as_object())
            .expect("properties.xs");
        let items = xs_schema.get("items").expect("properties.xs.items");
        assert_eq!(
            items,
            &serde_json::json!({}),
            "properties.xs.items must be normalised to `{{}}`, was {items}"
        );
        let value = serde_json::Value::Object((*schema).clone());
        let offenders = find_bool_subschemas(&value, "root");
        assert!(
            offenders.is_empty(),
            "no bare bool subschemas allowed; offenders: {offenders:?}"
        );
    }

    #[derive(serde::Serialize, serde::Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct OptionValueHolder {
        v: Option<serde_json::Value>,
    }

    /// `Option<serde_json::Value>` field — no bare boolean appears
    /// anywhere in the tree, regardless of the exact shape schemars
    /// emits for the `Option<T>` wrapper.
    #[test]
    fn option_value_normalised() {
        let schema = schema_for_type::<OptionValueHolder>();
        let value = serde_json::Value::Object((*schema).clone());
        let offenders = find_bool_subschemas(&value, "root");
        assert!(
            offenders.is_empty(),
            "no bare bool subschemas allowed; offenders: {offenders:?}; \
             schema: {value}"
        );
    }

    #[derive(serde::Serialize, serde::Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct MapValueHolder {
        m: std::collections::BTreeMap<String, serde_json::Value>,
    }

    /// `BTreeMap<String, serde_json::Value>` natively emits
    /// `additionalProperties: true` — the [#50194] reproducer trigger.
    /// Must normalise to `additionalProperties: {}`.
    #[test]
    fn map_value_additional_properties_normalised() {
        let schema = schema_for_type::<MapValueHolder>();
        let m_schema = schema
            .get("properties")
            .and_then(|v| v.as_object())
            .and_then(|p| p.get("m"))
            .and_then(|v| v.as_object())
            .expect("properties.m");
        let ap = m_schema
            .get("additionalProperties")
            .expect("properties.m.additionalProperties");
        assert_eq!(
            ap,
            &serde_json::json!({}),
            "properties.m.additionalProperties must be `{{}}` not `true`, was {ap}"
        );
    }

    #[derive(serde::Serialize, serde::Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct DenyUnknownFieldsStruct {
        x: i32,
    }

    /// `#[serde(deny_unknown_fields)]` produces `additionalProperties:
    /// false` natively; post-transform it becomes
    /// `additionalProperties: {"not": {}}` — semantically identical per
    /// JSON Schema 2020-12 §4.3.2 but in object form so the Claude Code
    /// walker tolerates it.
    #[test]
    fn deny_unknown_fields_normalised_to_not_empty_object() {
        let schema = schema_for_type::<DenyUnknownFieldsStruct>();
        let ap = schema
            .get("additionalProperties")
            .expect("root.additionalProperties");
        assert_eq!(
            ap,
            &serde_json::json!({ "not": {} }),
            "root.additionalProperties must be `{{\"not\":{{}}}}` not `false`, was {ap}"
        );
    }

    #[derive(serde::Serialize, serde::Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct InnerWithValue {
        v: serde_json::Value,
    }

    #[derive(serde::Serialize, serde::Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct OuterWithInner {
        inner: InnerWithValue,
    }

    /// Nested `Value` field — schemars typically emits `InnerWithValue`
    /// as a `$defs` entry referenced from `properties.inner`. Either
    /// inline or `$ref` shape, no bare bool may survive.
    #[test]
    fn nested_value_inside_struct() {
        let schema = schema_for_type::<OuterWithInner>();
        let value = serde_json::Value::Object((*schema).clone());
        let offenders = find_bool_subschemas(&value, "root");
        assert!(
            offenders.is_empty(),
            "no bare bool subschemas allowed in nested struct; offenders: \
             {offenders:?}; schema: {value}"
        );
    }

    #[derive(serde::Serialize, serde::Deserialize, JsonSchema)]
    #[allow(dead_code)]
    struct DefsHostA {
        a: ValueFieldHolder,
        b: ValueFieldHolder,
    }

    /// Force a `$defs` entry containing a `Value` field by reusing the
    /// same nested type twice. `$defs.<X>.properties.<f>` must be `{}`,
    /// not `true`.
    #[test]
    fn defs_subschema_booleans_normalised() {
        let schema = schema_for_type::<DefsHostA>();
        let value = serde_json::Value::Object((*schema).clone());
        let offenders = find_bool_subschemas(&value, "root");
        assert!(
            offenders.is_empty(),
            "no bare bool subschemas allowed in $defs; offenders: \
             {offenders:?}; schema: {value}"
        );
    }

    /// `Option<Value>` is the easiest enum-style construction whose
    /// schemars output may contain `oneOf`/`anyOf` arms with bare
    /// booleans. Walk every arm and assert each is an object.
    #[test]
    fn oneof_anyof_allof_arms_normalised() {
        let schema = schema_for_type::<OptionValueHolder>();
        let value = serde_json::Value::Object((*schema).clone());
        // All schema-array slots have already been walked by
        // `find_bool_subschemas`; this test specifically focuses on
        // pre-rendering the schema for visual inspection in failure logs.
        let offenders = find_bool_subschemas(&value, "root");
        assert!(
            offenders.is_empty(),
            "no bare bool in oneOf/anyOf/allOf arms; offenders: \
             {offenders:?}; schema: {value}"
        );
    }

    /// Cache hit returns the same `Arc` and the post-transform schema
    /// has no bare booleans. Then manually re-running the transform
    /// produces no further mutation (idempotent).
    #[test]
    fn idempotent() {
        let schema1 = schema_for_type::<ValueFieldHolder>();
        let schema2 = schema_for_type::<ValueFieldHolder>();
        assert!(
            Arc::ptr_eq(&schema1, &schema2),
            "cache must return the same Arc"
        );

        // Round-trip through `schemars::Schema` and re-apply the
        // transform — should be a no-op since `{}` and `{"not": {}}`
        // are not booleans.
        let value_before = serde_json::Value::Object((*schema1).clone());
        let mut as_schema: schemars::Schema = value_before
            .clone()
            .try_into()
            .expect("post-transform schema must be a valid object schema");
        let mut replace = schemars::transform::ReplaceBoolSchemas::default();
        schemars::transform::transform_subschemas(&mut replace, &mut as_schema);
        let value_after = serde_json::to_value(&as_schema).expect("serialize back");
        assert_eq!(
            value_before, value_after,
            "second pass of ReplaceBoolSchemas must be a no-op"
        );
    }

    /// Root rejection of degenerate `Json<serde_json::Value>`.
    ///
    /// `schemars::SchemaGenerator::into_root_schema_for` always calls
    /// `schema.ensure_object()` on the root before adding `$schema` /
    /// `title` / `$defs`, so the root is guaranteed to be an object —
    /// our `transform_subschemas`-only normalisation never sees a bool
    /// root in the type-derived path. For `T = serde_json::Value` the
    /// resulting object lacks `"type": "object"` (it's just `$schema`
    /// and `title: "AnyValue"`), so `schema_for_output` rejects it on
    /// the existing root-type check. Confirms the fix doesn't
    /// accidentally rescue degenerate `Json<Value>` outputs.
    #[test]
    fn degenerate_json_value_output_still_rejected() {
        let result = schema_for_output::<serde_json::Value>();
        assert!(
            result.is_err(),
            "schema_for_output::<serde_json::Value>() must reject — root \
             has no `\"type\": \"object\"`. Got: {result:?}"
        );
    }

    /// End-to-end: `schema_for_output::<Struct { v: Value }>()` produces
    /// an `outputSchema` with no bare booleans anywhere. Mirrors the
    /// per-MCP `no_tool_schema_emits_boolean_subschemas` tripwire.
    #[test]
    fn output_schema_walk_has_no_boolean_subschemas() {
        let result =
            schema_for_output::<ValueFieldHolder>().expect("ValueFieldHolder is an object schema");
        let value = serde_json::Value::Object((*result).clone());
        let offenders = find_bool_subschemas(&value, "outputSchema");
        assert!(
            offenders.is_empty(),
            "outputSchema must contain no bare bool subschemas; offenders: \
             {offenders:?}; schema: {value}"
        );
    }
}
