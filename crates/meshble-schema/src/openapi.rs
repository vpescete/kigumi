//! OpenAPI 3.1 projection: the integration contract generated from the model catalog.
//!
//! This is what makes the framework "integrable": any third party consumes the models like any
//! documented REST API, and typed SDKs (TS/Python/Go) are generated from this spec with standard
//! tooling (openapi-generator) — no hand-written, drift-prone client.

use meshble_core::{FieldDef, FieldKind, ResolvedModel};
use serde_json::{json, Map, Value};

/// Builds a pretty-printed OpenAPI 3.1 document describing `models`.
pub fn openapi(models: &[&ResolvedModel]) -> String {
    let mut schemas = Map::new();
    let mut paths = Map::new();
    for m in models {
        schemas.insert(m.name.to_string(), model_schema(m));
        let base = format!("/api/{}", m.table);
        paths.insert(base.clone(), json!({ "get": list_op(m) }));
        paths.insert(format!("{base}/{{id}}"), json!({ "get": get_op(m) }));
    }
    let doc = json!({
        "openapi": "3.1.0",
        "info": { "title": "Meshble API", "version": "0.1.0" },
        "paths": Value::Object(paths),
        "components": { "schemas": Value::Object(schemas) },
    });
    serde_json::to_string_pretty(&doc).expect("serialize openapi")
}

fn ref_of(m: &ResolvedModel) -> String {
    format!("#/components/schemas/{}", m.name)
}

fn model_schema(m: &ResolvedModel) -> Value {
    let mut props = Map::new();
    props.insert("id".into(), json!({ "type": "integer", "format": "int64", "readOnly": true }));
    let mut required = vec![Value::from("id")];
    for f in &m.fields {
        props.insert(f.name.to_string(), field_schema(f));
        if f.required {
            required.push(Value::from(f.name));
        }
    }
    json!({ "type": "object", "properties": Value::Object(props), "required": required })
}

fn field_schema(f: &FieldDef) -> Value {
    let mut s = match &f.kind {
        FieldKind::Text => json!({ "type": "string" }),
        FieldKind::Selection(opts) => {
            let variants: Vec<Value> = opts.iter().map(|(k, _)| Value::from(*k)).collect();
            json!({ "type": "string", "enum": variants })
        }
        FieldKind::Integer | FieldKind::Many2one { .. } => {
            json!({ "type": "integer", "format": "int64" })
        }
        // Exact decimals are serialized as strings to preserve precision (NUMERIC, not float).
        FieldKind::Decimal { .. } => json!({ "type": "string", "format": "decimal" }),
        FieldKind::Bool => json!({ "type": "boolean" }),
        // The get-one response inlines One2many children as full child objects, so the schema
        // references the child model (not a bare id array).
        FieldKind::One2many { target, .. } => {
            json!({ "type": "array", "items": { "$ref": format!("#/components/schemas/{target}") } })
        }
    };
    let obj = s.as_object_mut().expect("field schema is an object");
    obj.insert("title".into(), Value::from(f.label));
    if f.is_computed() {
        obj.insert("readOnly".into(), Value::from(true));
    }
    s
}

fn list_op(m: &ResolvedModel) -> Value {
    json!({
        "summary": format!("List {}", m.name),
        "operationId": format!("list_{}", m.table),
        "responses": { "200": { "description": "OK", "content": { "application/json": {
            "schema": { "type": "array", "items": { "$ref": ref_of(m) } } } } } }
    })
}

fn get_op(m: &ResolvedModel) -> Value {
    json!({
        "summary": format!("Get one {}", m.name),
        "operationId": format!("get_{}", m.table),
        "parameters": [ { "name": "id", "in": "path", "required": true,
            "schema": { "type": "integer", "format": "int64" } } ],
        "responses": { "200": { "description": "OK", "content": { "application/json": {
            "schema": { "$ref": ref_of(m) } } } } }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use meshble_core::{resolve, ModelDescriptor};

    static M: ModelDescriptor = ModelDescriptor {
        name: "sale.order",
        table: "sale_order",
        fields: &[
            FieldDef {
                name: "name", label: "Order Reference", kind: FieldKind::Text,
                required: true, stored: true, compute: None, depends: &[],
            },
            FieldDef {
                name: "state", label: "State",
                kind: FieldKind::Selection(&[("draft", "Draft"), ("done", "Done")]),
                required: true, stored: true, compute: None, depends: &[],
            },
            FieldDef {
                name: "line_ids", label: "Lines",
                kind: FieldKind::One2many { target: "sale.order.line", inverse: "order_id" },
                required: false, stored: false, compute: None, depends: &[],
            },
        ],
    };

    #[test]
    fn generates_valid_openapi_3_1() {
        let m = resolve(&M, &[]).unwrap();
        let spec = openapi(&[&m]);
        // Must be valid JSON and a well-formed OpenAPI 3.1 document.
        let v: Value = serde_json::from_str(&spec).unwrap();
        assert_eq!(v["openapi"], "3.1.0");
        assert!(v["components"]["schemas"]["sale.order"]["properties"]["state"]["enum"].is_array());
        assert!(v["paths"]["/api/sale_order"]["get"].is_object());
        assert_eq!(
            v["paths"]["/api/sale_order/{id}"]["get"]["operationId"],
            "get_sale_order"
        );
        // One2many is schema'd as an array of the CHILD object (matches the inlined get-one read).
        assert_eq!(
            v["components"]["schemas"]["sale.order"]["properties"]["line_ids"]["items"]["$ref"],
            "#/components/schemas/sale.order.line"
        );
    }
}
