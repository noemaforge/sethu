//! Convert OpenAPI 3.0 schemas into JSON Schema, per direction.
//!
//! OpenAPI 3.0 schemas are not JSON Schema, and validation depends on
//! direction. A request body must not be forced to carry `readOnly`
//! properties, while a response body must not be forced to carry
//! `writeOnly` ones. Nullability also differs from JSON Schema: version
//! 3.0 spells it `nullable: true`, either beside an explicit `type` or as
//! a reference idiom with no `type` at all. Boolean
//! `exclusiveMinimum` and `exclusiveMaximum` modify `minimum` and
//! `maximum` instead of holding bounds themselves.
//!
//! [`convert_named`] converts one named component of a parsed spec, and
//! [`convert_schema`] converts one schema value. Both take a validation
//! [`Direction`], rewrite local references into a local definitions map,
//! record every applied rule with its location, and report unsupported
//! constructs instead of passing them silently. References resolve from
//! the given spec only. Nothing is fetched from the network.
//!
//! Spec and schema content travels as plain JSON values. Schemas are data,
//! so loose values are the honest shape here. Every other shape in this
//! module is a named struct or enum.

use std::collections::HashSet;

use anyhow::Context;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

/// JSON Schema draft the converted documents declare.
///
/// The converted bounds and type unions match this draft. Validation uses
/// it explicitly through [`ConvertedSchema::document`].
pub const DRAFT_2020_12: &str = "https://json-schema.org/draft/2020-12/schema";

/// Validation direction for one conversion.
///
/// Request and response validation share every rule except the access
/// markers. Requests drop `readOnly` properties from `required`, and
/// responses drop `writeOnly` ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Validate a request body.
    Request,
    /// Validate a response body.
    Response,
}

/// One conversion rule that fired during schema conversion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rule {
    /// `nullable: true` beside an explicit `type` widened it with `"null"`.
    NullableType,
    /// The nullable reference idiom became an `anyOf` with null.
    NullableReference,
    /// A `readOnly` property left `required` for request validation.
    ReadOnly,
    /// A `writeOnly` property left `required` for response validation.
    WriteOnly,
    /// Boolean `exclusiveMinimum` became a numeric bound.
    ExclusiveMinimum,
    /// Boolean `exclusiveMaximum` became a numeric bound.
    ExclusiveMaximum,
    /// A local `$ref` moved to the local definitions map.
    LocalRef,
}

/// One recorded rule application at one schema location.
///
/// Locations are JSON pointers into the converted document. The root is
/// `#`, a property is `#/properties/stack`, and a resolved component
/// member is `#/definitions/Name/properties/id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Application {
    /// Pointer to the converted node the rule applied to.
    pub location: String,
    /// Rule that fired at that location.
    pub rule: Rule,
    /// Short factual note, such as the dropped property name.
    pub detail: String,
}

/// One unsupported construct found during conversion.
///
/// A non-empty list means the converted schema cannot back a verification
/// claim on its own. Callers report these instead of passing them
/// silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedKind {
    /// A `discriminator` keyword, which needs runtime dispatch.
    Discriminator,
    /// `nullable: true` without an explicit `type` and outside the idiom.
    NullableWithoutType,
    /// A `$ref` pointing outside the local document.
    ExternalRef,
    /// A `format` value outside the validator built-in set.
    Format,
    /// An `xml` keyword, which has no JSON meaning.
    Xml,
}

/// One unsupported construct at one schema location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsupportedIssue {
    /// Pointer to the converted node carrying the construct.
    pub location: String,
    /// Kind of unsupported construct.
    pub kind: UnsupportedKind,
    /// Short factual note, such as the offending reference or format.
    pub detail: String,
}

/// One converted schema plus its derivation record.
///
/// The root holds the converted schema with local references rewritten to
/// `#/definitions/...`. The map holds each reachable component under its
/// plain name, converted with the same direction. Applications record
/// every fired rule in conversion order. Unsupported names every blocking
/// construct in conversion order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConvertedSchema {
    /// Converted root schema with rewritten references.
    pub schema: serde_json::Value,
    /// Converted reachable components in first-seen order.
    pub definitions: IndexMap<String, serde_json::Value>,
    /// Fired rules in conversion order.
    pub applications: Vec<Application>,
    /// Blocking constructs in conversion order.
    pub unsupported: Vec<UnsupportedIssue>,
}

impl ConvertedSchema {
    /// Merge the converted root and its definitions into one document.
    ///
    /// The document declares draft 2020-12 first, then carries the root
    /// keys, then the definitions map. A root that is not an object comes
    /// back unchanged, since boolean schemas are already valid. Converted
    /// entries win when the root already carries a `definitions` key.
    pub fn document(&self) -> serde_json::Value {
        let Some(object) = self.schema.as_object() else {
            return self.schema.clone();
        };
        let mut out = serde_json::Map::with_capacity(object.len() + 2);
        out.insert(
            "$schema".to_string(),
            serde_json::Value::String(DRAFT_2020_12.to_string()),
        );
        for (key, value) in object {
            out.insert(key.clone(), value.clone());
        }
        if !self.definitions.is_empty() {
            let mut merged = match out.remove("definitions") {
                Some(serde_json::Value::Object(map)) => map,
                _ => serde_json::Map::new(),
            };
            for (name, schema) in &self.definitions {
                merged.insert(name.clone(), schema.clone());
            }
            out.insert("definitions".to_string(), serde_json::Value::Object(merged));
        }
        serde_json::Value::Object(out)
    }

    /// Report whether the conversion met only supported constructs.
    pub fn supports_claim(&self) -> bool {
        self.unsupported.is_empty()
    }
}

/// Named component schemas of one OpenAPI document, for `$ref` resolution.
///
/// Built from the parsed spec kept as data. Resolution reads this index
/// only and never touches the network.
#[derive(Debug, Clone, Default)]
pub struct SpecIndex {
    /// Component schemas by plain name, in document order.
    schemas: IndexMap<String, serde_json::Value>,
}

impl SpecIndex {
    /// Collect the component schemas of one parsed OpenAPI document.
    ///
    /// Reads `components.schemas`, with a fallback to a top-level
    /// `definitions` map. The fallback covers older documents that keep
    /// schemas there. A missing map yields an empty index rather than an
    /// error, so reference-free schemas still convert.
    pub fn from_spec(spec: &serde_json::Value) -> anyhow::Result<Self> {
        let mut schemas = IndexMap::new();
        if let Some(definitions) = spec.get("definitions") {
            let map = definitions
                .as_object()
                .with_context(|| "read `definitions` as an object")?;
            for (name, schema) in map {
                schemas.insert(name.clone(), schema.clone());
            }
        }
        if let Some(components) = spec.get("components")
            && let Some(nested) = components.get("schemas")
        {
            let map = nested
                .as_object()
                .with_context(|| "read `components.schemas` as an object")?;
            for (name, schema) in map {
                schemas.insert(name.clone(), schema.clone());
            }
        }
        Ok(Self { schemas })
    }

    /// Look up one component schema by plain name.
    pub fn get(&self, name: &str) -> Option<&serde_json::Value> {
        self.schemas.get(name)
    }
}

/// Convert one named component of the indexed spec.
///
/// The root converts at location `#`. Reachable components convert under
/// `#/definitions/...`. Fails when the name is unknown, when a local
/// reference points outside the component maps, or when a reachable
/// component is missing.
pub fn convert_named(
    index: &SpecIndex,
    name: &str,
    direction: Direction,
) -> anyhow::Result<ConvertedSchema> {
    let raw = index
        .get(name)
        .with_context(|| format!("find schema {name:?} in the spec index"))?;
    let mut converter = Converter::new(direction, index);
    let schema = converter.convert_node(raw, "#")?;
    converter.finish(schema, Some(name))
}

/// Convert one schema value against the indexed spec.
///
/// The value converts at location `#`, and any component it references
/// resolves through the index. Useful for request and response schemas
/// taken from outside the component maps.
pub fn convert_schema(
    schema: &serde_json::Value,
    index: &SpecIndex,
    direction: Direction,
) -> anyhow::Result<ConvertedSchema> {
    let mut converter = Converter::new(direction, index);
    let converted = converter.convert_node(schema, "#")?;
    converter.finish(converted, None)
}

/// Validator format names the converter passes through quietly.
///
/// These match the built-in set of the JSON Schema validator. Any other
/// `format` value is kept in the output but reported as unsupported, since
/// the validator cannot check it.
const BUILT_IN_FORMATS: &[&str] = &[
    "date",
    "date-time",
    "duration",
    "email",
    "hostname",
    "idn-email",
    "idn-hostname",
    "ipv4",
    "ipv6",
    "iri",
    "iri-reference",
    "json-pointer",
    "regex",
    "relative-json-pointer",
    "time",
    "uri",
    "uri-reference",
    "uri-template",
    "uuid",
];

/// Report whether a format name belongs to the validator built-in set.
fn is_builtin_format(name: &str) -> bool {
    BUILT_IN_FORMATS.contains(&name)
}

/// Report whether a reference points outside the local document.
fn is_external(reference: &str) -> bool {
    !reference.starts_with("#/")
}

/// Read a component name from a local reference.
///
/// Accepts `#/components/schemas/Name` and `#/definitions/Name` with
/// pointer unescaping. Anything else yields none, so callers can fail
/// loudly instead of guessing.
fn component_name(reference: &str) -> Option<String> {
    let rest = reference
        .strip_prefix("#/components/schemas/")
        .or_else(|| reference.strip_prefix("#/definitions/"))?;
    if rest.is_empty() || rest.contains('/') {
        return None;
    }
    Some(rest.replace("~1", "/").replace("~0", "~"))
}

/// Match the nullable reference idiom and return its target.
///
/// The idiom holds exactly one `allOf` entry carrying a single `$ref`,
/// plus `nullable: true` and nothing else. Generators emit this shape for
/// a nullable reference. Anything wider falls through to the generic
/// nullable handling.
fn idiom_target(object: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    if object.get("nullable").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    if object.len() != 2 {
        return None;
    }
    let members = object.get("allOf")?.as_array()?;
    if members.len() != 1 {
        return None;
    }
    let first = members[0].as_object()?;
    if first.len() != 1 {
        return None;
    }
    Some(first.get("$ref")?.as_str()?.to_string())
}

/// Widen a `type` value with `"null"`.
///
/// A plain name becomes a two-item union. An existing union gains the
/// null member unless it already holds one. Anything else comes back
/// unchanged.
fn widen_type(current: &serde_json::Value) -> serde_json::Value {
    let null = serde_json::Value::String("null".to_string());
    match current {
        serde_json::Value::String(name) => {
            if name == "null" {
                current.clone()
            } else {
                serde_json::Value::Array(vec![current.clone(), null])
            }
        }
        serde_json::Value::Array(items) => {
            if items.iter().any(|item| item.as_str() == Some("null")) {
                current.clone()
            } else {
                let mut next = items.clone();
                next.push(null);
                serde_json::Value::Array(next)
            }
        }
        _ => current.clone(),
    }
}

/// Join a child token onto a pointer location with pointer escaping.
fn child(location: &str, token: &str) -> String {
    let escaped = token.replace('~', "~0").replace('/', "~1");
    format!("{location}/{escaped}")
}

/// One running conversion with its collected record.
struct Converter<'a> {
    /// Direction under conversion.
    direction: Direction,
    /// Component lookup for reference resolution.
    index: &'a SpecIndex,
    /// Fired rules in conversion order.
    applications: Vec<Application>,
    /// Blocking constructs in conversion order.
    unsupported: Vec<UnsupportedIssue>,
    /// Referenced components in first-seen order.
    scheduled: Vec<String>,
    /// Referenced components already queued.
    known: HashSet<String>,
}

impl<'a> Converter<'a> {
    /// Start one conversion under the given direction and index.
    fn new(direction: Direction, index: &'a SpecIndex) -> Self {
        Self {
            direction,
            index,
            applications: Vec::new(),
            unsupported: Vec::new(),
            scheduled: Vec::new(),
            known: HashSet::new(),
        }
    }

    /// Record one fired rule at one location.
    fn record(&mut self, location: &str, rule: Rule, detail: String) {
        self.applications.push(Application {
            location: location.to_string(),
            rule,
            detail,
        });
    }

    /// Report one unsupported construct at one location.
    fn flag(&mut self, location: &str, kind: UnsupportedKind, detail: String) {
        self.unsupported.push(UnsupportedIssue {
            location: location.to_string(),
            kind,
            detail,
        });
    }

    /// Queue one referenced component, failing when it is unknown.
    fn schedule(&mut self, name: &str, location: &str) -> anyhow::Result<()> {
        if self.index.get(name).is_none() {
            anyhow::bail!("unknown schema {name:?} referenced from {location}");
        }
        if self.known.insert(name.to_string()) {
            self.scheduled.push(name.to_string());
        }
        Ok(())
    }

    /// Convert the converted root and queued components into the output.
    ///
    /// Queued components convert in first-seen order, and the queue may
    /// grow while they convert. A root that references itself also lands
    /// in the definitions map, so recursive schemas keep a target.
    fn finish(
        mut self,
        schema: serde_json::Value,
        root_name: Option<&str>,
    ) -> anyhow::Result<ConvertedSchema> {
        let index = self.index;
        let mut definitions = IndexMap::new();
        let mut cursor = 0;
        while cursor < self.scheduled.len() {
            let next = self.scheduled[cursor].clone();
            cursor += 1;
            if root_name == Some(next.as_str()) || definitions.contains_key(&next) {
                continue;
            }
            let raw = index
                .get(&next)
                .with_context(|| format!("find schema {next:?} referenced during conversion"))?;
            let location = format!("#/definitions/{next}");
            let converted = self.convert_node(raw, &location)?;
            definitions.insert(next, converted);
        }
        if let Some(name) = root_name
            && self.scheduled.iter().any(|seen| seen == name)
        {
            definitions.insert(name.to_string(), schema.clone());
        }
        Ok(ConvertedSchema {
            schema,
            definitions,
            applications: self.applications,
            unsupported: self.unsupported,
        })
    }

    /// Convert any value, recursing into objects and arrays.
    fn convert_value(
        &mut self,
        value: &serde_json::Value,
        location: &str,
    ) -> anyhow::Result<serde_json::Value> {
        match value {
            serde_json::Value::Object(_) => self.convert_node(value, location),
            serde_json::Value::Array(items) => {
                let mut out = Vec::with_capacity(items.len());
                for (position, item) in items.iter().enumerate() {
                    out.push(self.convert_value(item, &child(location, &position.to_string()))?);
                }
                Ok(serde_json::Value::Array(out))
            }
            _ => Ok(value.clone()),
        }
    }

    /// Convert one schema object with direction-aware rules.
    ///
    /// Unsupported keywords are flagged and the conversion carries on, so
    /// one report covers the whole schema. Unknown component names and
    /// non-component local references fail, since guessing would rewrite
    /// the contract.
    fn convert_node(
        &mut self,
        node: &serde_json::Value,
        location: &str,
    ) -> anyhow::Result<serde_json::Value> {
        let Some(object) = node.as_object() else {
            return Ok(node.clone());
        };
        if object.contains_key("discriminator") {
            self.flag(
                location,
                UnsupportedKind::Discriminator,
                "discriminator needs runtime dispatch".to_string(),
            );
        }
        if object.contains_key("xml") {
            self.flag(
                location,
                UnsupportedKind::Xml,
                "xml metadata has no JSON meaning".to_string(),
            );
        }
        if let Some(name) = object.get("format").and_then(serde_json::Value::as_str)
            && !is_builtin_format(name)
        {
            self.flag(
                location,
                UnsupportedKind::Format,
                format!("format {name:?} is not checked by the validator"),
            );
        }

        // Nullable handling runs before emission. An explicit `type`
        // widens, the reference idiom rewrites to `anyOf`, and anything
        // else is reported. The `nullable` key itself never survives,
        // since JSON Schema has no such keyword.
        let mut widened: Option<serde_json::Value> = None;
        let mut idiom: Option<String> = None;
        if object.get("nullable").and_then(serde_json::Value::as_bool) == Some(true) {
            if let Some(current) = object.get("type") {
                if current.is_string() || current.is_array() {
                    widened = Some(widen_type(current));
                    self.record(location, Rule::NullableType, "type allows null".to_string());
                } else {
                    self.flag(
                        location,
                        UnsupportedKind::NullableWithoutType,
                        "nullable beside a non-string type".to_string(),
                    );
                }
            } else if let Some(target) = idiom_target(object) {
                if component_name(&target).is_some() {
                    idiom = Some(target);
                } else {
                    self.flag(
                        location,
                        UnsupportedKind::NullableWithoutType,
                        "nullable reference outside the local components".to_string(),
                    );
                }
            } else {
                self.flag(
                    location,
                    UnsupportedKind::NullableWithoutType,
                    "nullable without a type or a single local reference".to_string(),
                );
            }
        }
        if let Some(target) = idiom {
            self.record(
                location,
                Rule::NullableReference,
                format!("nullable reference to {target} allows null"),
            );
            let fresh = serde_json::json!({ "anyOf": [{ "$ref": target }, { "type": "null" }] });
            return self.convert_node(&fresh, location);
        }

        // Reference rewriting runs next. External references stay as they
        // are and are reported. Anything local must name a component.
        let mut rewritten: Option<String> = None;
        if let Some(target) = object.get("$ref").and_then(serde_json::Value::as_str) {
            if is_external(target) {
                self.flag(
                    location,
                    UnsupportedKind::ExternalRef,
                    format!("external reference {target:?} stays unresolved"),
                );
            } else if let Some(name) = component_name(target) {
                self.schedule(&name, location)?;
                let moved = format!("#/definitions/{name}");
                self.record(
                    location,
                    Rule::LocalRef,
                    format!("{target} resolves to {moved}"),
                );
                rewritten = Some(moved);
            } else {
                anyhow::bail!("local reference {target:?} at {location} is not a component schema");
            }
        }

        // Boolean bounds become numeric ones. A true flag consumes its
        // `minimum` or `maximum` value. A false flag drops, as does a true
        // flag with no bound to carry. Numeric bounds already match JSON
        // Schema and pass through.
        let mut numeric_minimum: Option<serde_json::Value> = None;
        let mut numeric_maximum: Option<serde_json::Value> = None;
        let mut drop_minimum = false;
        let mut drop_maximum = false;
        if object
            .get("exclusiveMinimum")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            && let Some(bound) = object.get("minimum")
            && bound.is_number()
        {
            numeric_minimum = Some(bound.clone());
            drop_minimum = true;
            self.record(
                location,
                Rule::ExclusiveMinimum,
                format!("exclusive lower bound {bound}"),
            );
        }
        if object
            .get("exclusiveMaximum")
            .and_then(serde_json::Value::as_bool)
            == Some(true)
            && let Some(bound) = object.get("maximum")
            && bound.is_number()
        {
            numeric_maximum = Some(bound.clone());
            drop_maximum = true;
            self.record(
                location,
                Rule::ExclusiveMaximum,
                format!("exclusive upper bound {bound}"),
            );
        }

        // Access markers leave `required` per direction. Properties the
        // object does not declare stay required, since the converter
        // cannot judge them.
        let marker = match self.direction {
            Direction::Request => "readOnly",
            Direction::Response => "writeOnly",
        };
        let access_rule = match self.direction {
            Direction::Request => Rule::ReadOnly,
            Direction::Response => Rule::WriteOnly,
        };
        let mut filtered: Option<Vec<serde_json::Value>> = None;
        if let Some(list) = object.get("required").and_then(serde_json::Value::as_array) {
            let properties = object
                .get("properties")
                .and_then(serde_json::Value::as_object);
            let mut kept = Vec::with_capacity(list.len());
            let mut shaped = true;
            for entry in list {
                let Some(name) = entry.as_str() else {
                    shaped = false;
                    kept.push(entry.clone());
                    continue;
                };
                let hidden = properties
                    .and_then(|map| map.get(name))
                    .and_then(|property| property.get(marker))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                if hidden {
                    self.record(location, access_rule, format!("{name:?} leaves required"));
                } else {
                    kept.push(entry.clone());
                }
            }
            if shaped {
                filtered = Some(kept);
            }
        }

        let mut out = serde_json::Map::with_capacity(object.len());
        for (key, value) in object {
            match key.as_str() {
                "nullable" => {}
                // Map keys below hold schemas under arbitrary member
                // names. A member named `type` is a property, not the
                // type keyword, so each member converts generically.
                "properties" | "patternProperties" | "definitions" | "$defs"
                | "dependentSchemas" => {
                    if let Some(map) = value.as_object() {
                        let mut inner = serde_json::Map::with_capacity(map.len());
                        for (name, member) in map {
                            let member_location = child(&child(location, key), name);
                            inner.insert(
                                name.clone(),
                                self.convert_value(member, &member_location)?,
                            );
                        }
                        out.insert(key.clone(), serde_json::Value::Object(inner));
                    } else {
                        out.insert(
                            key.clone(),
                            self.convert_value(value, &child(location, key))?,
                        );
                    }
                }
                "type" => {
                    out.insert(
                        key.clone(),
                        widened.clone().unwrap_or_else(|| value.clone()),
                    );
                }
                "$ref" => {
                    out.insert(
                        key.clone(),
                        rewritten
                            .clone()
                            .map(serde_json::Value::String)
                            .unwrap_or_else(|| value.clone()),
                    );
                }
                "required" => match &filtered {
                    Some(kept) => {
                        out.insert(key.clone(), serde_json::Value::Array(kept.clone()));
                    }
                    None => {
                        out.insert(
                            key.clone(),
                            self.convert_value(value, &child(location, key))?,
                        );
                    }
                },
                "exclusiveMinimum" => {
                    if let Some(bound) = &numeric_minimum {
                        out.insert(key.clone(), bound.clone());
                    } else if !value.is_boolean() {
                        out.insert(key.clone(), value.clone());
                    }
                }
                "exclusiveMaximum" => {
                    if let Some(bound) = &numeric_maximum {
                        out.insert(key.clone(), bound.clone());
                    } else if !value.is_boolean() {
                        out.insert(key.clone(), value.clone());
                    }
                }
                "minimum" if drop_minimum => {}
                "maximum" if drop_maximum => {}
                _ => {
                    out.insert(
                        key.clone(),
                        self.convert_value(value, &child(location, key))?,
                    );
                }
            }
        }
        Ok(serde_json::Value::Object(out))
    }
}
