use schemars::{JsonSchema, Schema, SchemaGenerator};
use serde_json::Value;

/// Builds the JSON Schema sent to providers as a structured-output constraint.
///
/// Derived schemas are normalized for the strict subsets providers accept:
/// `format`, numeric bounds, and schema identifiers are either unsupported or
/// irrelevant, so they are removed before the schema leaves this module.
pub fn response_schema<T: JsonSchema + ?Sized>() -> Schema {
    let schema = SchemaGenerator::default().into_root_schema_for::<T>();
    let mut value = schema.to_value();
    strip_unsupported_keywords(&mut value);
    Schema::try_from(value).expect("derived response schema root is an object")
}

const SCHEMA_MAPS: [&str; 5] = [
    "$defs",
    "definitions",
    "properties",
    "patternProperties",
    "dependentSchemas",
];
const SCHEMA_SINGLE: [&str; 8] = [
    "items",
    "additionalProperties",
    "propertyNames",
    "contains",
    "not",
    "if",
    "then",
    "else",
];
const SCHEMA_ARRAYS: [&str; 4] = ["anyOf", "allOf", "oneOf", "prefixItems"];

fn strip_unsupported_keywords(schema: &mut Value) {
    let Some(object) = schema.as_object_mut() else {
        return;
    };
    for keyword in ["format", "$schema", "$id", "minimum", "maximum"] {
        object.remove(keyword);
    }
    // Property names are arbitrary, so they are only entered through the
    // keyword maps that hold subschemas.
    for keyword in SCHEMA_MAPS {
        if let Some(Value::Object(map)) = object.get_mut(keyword) {
            for child in map.values_mut() {
                strip_unsupported_keywords(child);
            }
        }
    }
    for keyword in SCHEMA_SINGLE {
        if let Some(child) = object.get_mut(keyword) {
            strip_unsupported_keywords(child);
        }
    }
    for keyword in SCHEMA_ARRAYS {
        if let Some(Value::Array(children)) = object.get_mut(keyword) {
            for child in children {
                strip_unsupported_keywords(child);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::response_schema;
    use schemars::JsonSchema;
    use serde::Deserialize;

    #[derive(Deserialize, JsonSchema)]
    #[schemars(inline)]
    #[allow(dead_code)]
    struct Sample {
        chapter: usize,
        title: String,
    }

    #[test]
    fn derived_schema_drops_keywords_providers_reject() {
        let schema = response_schema::<Sample>().to_value();
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["properties"]["chapter"]["type"], "integer");
        assert!(schema["properties"]["chapter"].get("format").is_none());
        assert!(schema.get("$schema").is_none());
    }

    #[test]
    fn property_names_are_not_treated_as_keywords() {
        #[derive(Deserialize, JsonSchema)]
        #[allow(dead_code)]
        struct WithFormatField {
            format: String,
        }
        let schema = response_schema::<WithFormatField>().to_value();
        assert!(schema["properties"].get("format").is_some());
    }
}
