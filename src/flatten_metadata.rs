//! Project standardized filing metadata into six accession-linked business tables.
use anyhow::{Result, bail};
use arrow_array::builder::{ListBuilder, StringBuilder};
use arrow_array::{ArrayRef, RecordBatch, UInt64Array};
use arrow_schema::{DataType, Field, Schema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, OnceLock};

const TABLES: [&str; 6] = [
    "filings",
    "documents",
    "entities",
    "former_companies",
    "series",
    "classes",
];
const ROLES: [&str; 11] = [
    "filer",
    "reporting-owner",
    "issuer",
    "subject-company",
    "filed-by",
    "filed-for",
    "securitizer",
    "depositor",
    "underwriter",
    "issuing-entity",
    "serial-company",
];
const KEYS: &[&str] = &[
    "documents",
    "former-company",
    "former-company-data",
    "former-name",
    "series",
    "new-series",
    "class-contract",
    "paper",
    "company-data",
    "owner-data",
    "business-address",
    "mail-address",
    "filing-values",
    "assigned-sic",
    "conformed-name",
    "organization-name",
    "cik",
    "ein",
    "irs-number",
    "fiscal-year-end",
    "state-of-incorporation",
    "relationship",
    "file-number",
    "film-number",
    "form-type",
    "date-changed",
    "former-conformed-name",
    "series-id",
    "owner-cik",
    "class-contract-id",
    "class-contract-name",
    "class-contract-ticker-symbol",
    "accession-number",
    "ma-i-individual",
    "act",
    "type",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MetadataValue {
    String(String),
    List(Vec<Option<String>>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataRow {
    #[serde(rename = "accessionNumber")]
    pub accession_number: u64,
    #[serde(flatten)]
    pub values: BTreeMap<String, MetadataValue>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetadataTable {
    pub name: String,
    pub rows: Vec<MetadataRow>,
}

impl MetadataTable {
    /// Integer accession plus only populated columns, with stable scalar/list types.
    pub fn to_record_batch(&self) -> Result<RecordBatch> {
        let mut fields = vec![Field::new("accessionNumber", DataType::UInt64, false)];
        let mut arrays: Vec<ArrayRef> = vec![Arc::new(UInt64Array::from_iter_values(
            self.rows.iter().map(|r| r.accession_number),
        ))];
        let columns: BTreeMap<String, bool> = self
            .rows
            .iter()
            .flat_map(|r| {
                r.values
                    .iter()
                    .map(|(k, v)| (k.clone(), matches!(v, MetadataValue::List(_))))
            })
            .collect();
        for (name, list) in columns {
            if list {
                let child = Arc::new(Field::new("element", DataType::Utf8, true));
                let mut builder = ListBuilder::new(StringBuilder::new()).with_field(child.clone());
                for row in &self.rows {
                    match row.values.get(&name) {
                        Some(MetadataValue::List(values)) => {
                            for value in values {
                                builder.values().append_option(value.as_deref());
                            }
                            builder.append(true);
                        }
                        None => builder.append(false),
                        _ => bail!("inconsistent list type for {name}"),
                    }
                }
                fields.push(Field::new(&name, DataType::List(child), true));
                arrays.push(Arc::new(builder.finish()));
            } else {
                let mut builder = StringBuilder::new();
                for row in &self.rows {
                    match row.values.get(&name) {
                        Some(MetadataValue::String(value)) => builder.append_value(value),
                        None => builder.append_null(),
                        _ => bail!("inconsistent scalar type for {name}"),
                    }
                }
                fields.push(Field::new(&name, DataType::Utf8, true));
                arrays.push(Arc::new(builder.finish()));
            }
        }
        Ok(RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays)?)
    }
}

#[derive(Deserialize)]
struct Model {
    fields: Vec<FieldSpec>,
}
#[derive(Deserialize)]
struct FieldSpec {
    field: String,
    storage: String,
    column: String,
}
type Lookup = BTreeMap<String, BTreeMap<(String, bool), String>>;
fn lookup() -> &'static Lookup {
    static LOOKUP: OnceLock<Lookup> = OnceLock::new();
    LOOKUP.get_or_init(|| {
        let models: BTreeMap<String, Model> =
            serde_json::from_str(include_str!("../assets/filing_metadata_fields.json"))
                .expect("valid bundled metadata fields");
        models
            .into_iter()
            .map(|(table, model)| {
                let mut fields = BTreeMap::new();
                for field in model.fields {
                    let list = field.storage == "list";
                    fields.insert((field.field.clone(), list), field.column.clone());
                    // Known fields can arrive as strings or lists, including future alias conflicts.
                    if field.field != "@entity_cik" {
                        let other = if list {
                            field
                                .column
                                .strip_suffix("__list")
                                .unwrap_or(&field.column)
                                .to_string()
                        } else {
                            format!("{}__list", field.column)
                        };
                        fields.entry((field.field, !list)).or_insert(other);
                    }
                }
                (table, fields)
            })
            .collect()
    })
}

#[derive(Default)]
struct RawRow {
    table: usize,
    context: BTreeMap<String, String>,
    primary_cik: Option<Vec<String>>,
    fields: BTreeMap<(String, bool), Option<MetadataValue>>,
    merged: HashSet<String>,
}

fn canonical(key: &str) -> String {
    let normalized = key.to_lowercase().replace('_', "-");
    if ROLES.contains(&normalized.as_str()) || KEYS.contains(&normalized.as_str()) {
        normalized
    } else {
        key.to_string()
    }
}
fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(v) => Some(v.clone()),
        Value::Bool(v) => Some(v.to_string()),
        Value::Number(v) => Some(v.to_string()),
        _ => None,
    }
}
fn named<'a>(value: &'a Value, name: &str) -> Option<&'a Value> {
    value
        .as_object()?
        .iter()
        .find(|(k, _)| canonical(k) == name)
        .map(|(_, v)| v)
}
fn destination(name: &str) -> Option<usize> {
    match name {
        "documents" => Some(1),
        n if ROLES.contains(&n) => Some(2),
        "former-company" | "former-company-data" | "former-name" => Some(3),
        "series" | "new-series" => Some(4),
        "class-contract" => Some(5),
        _ => None,
    }
}
fn primary_cik(value: &Value) -> Option<(String, Vec<String>)> {
    for group in ["company-data", "owner-data", ""] {
        let object = value.as_object()?;
        let candidates: Vec<(&str, &Value)> = if group.is_empty() {
            vec![("", value)]
        } else {
            object
                .iter()
                .filter(|(k, _)| canonical(k) == group)
                .map(|(k, v)| (k.as_str(), v))
                .collect()
        };
        for (key, node) in candidates {
            if let Some(object) = node.as_object() {
                for (k, v) in object {
                    if canonical(k) == "cik" {
                        if let Some(cik) = scalar(v) {
                            return Some((
                                cik,
                                if key.is_empty() {
                                    vec![k.clone()]
                                } else {
                                    vec![key.to_string(), k.clone()]
                                },
                            ));
                        }
                    }
                }
            }
        }
    }
    None
}
fn accession_value(value: &Value) -> Option<u64> {
    let text = scalar(value)?;
    let digits = text.replace('-', "");
    if digits.is_empty() || digits.len() > 18 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

struct Projection {
    accession: u64,
    root_keys: HashSet<String>,
    rows: Vec<RawRow>,
}
impl Projection {
    fn store(
        &mut self,
        index: Option<usize>,
        mut relative: Vec<String>,
        original: &[String],
        value: Option<MetadataValue>,
    ) {
        let Some(index) = index else {
            return;
        };
        let row = &mut self.rows[index];
        if row.table == 2
            && relative.len() >= 3
            && ["company-data", "owner-data"].contains(&relative[0].as_str())
        {
            if ["business-address", "mail-address", "filing-values"].contains(&relative[1].as_str())
            {
                relative.remove(0);
            } else if ["assigned-sic", "organization-name"].contains(&relative[1].as_str())
                && [
                    "fiscal-year-end",
                    "irs-number",
                    "state-of-incorporation",
                    "relationship",
                ]
                .contains(&relative[2].as_str())
            {
                relative.remove(1);
            }
        }
        if row.table == 0
            && relative.len() > 1
            && relative[0] == "paper"
            && !self.root_keys.contains(&relative[1])
        {
            relative.remove(0);
        }
        let field = if row.table == 2 && row.primary_cik.as_deref() == Some(original) {
            "@entity_cik".to_string()
        } else if relative.is_empty() {
            "/value".to_string()
        } else {
            relative
                .iter()
                .map(|k| format!("/{}", k.replace('~', "~0").replace('/', "~1")))
                .collect()
        };
        let list = matches!(value, Some(MetadataValue::List(_)));
        if list {
            let Some(MetadataValue::List(values)) = value else {
                unreachable!()
            };
            let existing = row
                .fields
                .entry((field, true))
                .or_insert_with(|| Some(MetadataValue::List(Vec::new())));
            if let Some(MetadataValue::List(existing)) = existing {
                existing.extend(values);
            }
        } else if row.merged.contains(&field) {
            if let Some(MetadataValue::String(value)) = value {
                if let Some(Some(MetadataValue::List(values))) = row.fields.get_mut(&(field, true))
                {
                    values.push(Some(value));
                }
            }
        } else if let Some(previous) = row.fields.remove(&(field.clone(), false)) {
            let values: Vec<Option<String>> = [previous, value]
                .into_iter()
                .flatten()
                .filter_map(|v| {
                    if let MetadataValue::String(v) = v {
                        Some(Some(v))
                    } else {
                        None
                    }
                })
                .collect();
            if values.len() < 2 {
                row.fields.insert(
                    (field, false),
                    values
                        .into_iter()
                        .next()
                        .flatten()
                        .map(MetadataValue::String),
                );
            } else {
                let existing = row
                    .fields
                    .entry((field.clone(), true))
                    .or_insert_with(|| Some(MetadataValue::List(Vec::new())));
                if let Some(MetadataValue::List(existing)) = existing {
                    existing.extend(values);
                }
                row.merged.insert(field);
            }
        } else {
            row.fields.insert((field, false), value);
        }
    }

    fn visit(
        &mut self,
        value: &Value,
        mut index: Option<usize>,
        mut relative: Vec<String>,
        mut context: BTreeMap<String, String>,
        name: &str,
        array_item: bool,
        mut original: Vec<String>,
    ) {
        match value {
            Value::Object(object) => {
                if let Some(table) = destination(name) {
                    let mut row = RawRow {
                        table,
                        context: context.clone(),
                        ..RawRow::default()
                    };
                    if table == 2 {
                        if let Some((cik, path)) = primary_cik(value) {
                            row.context.insert("entity_cik".to_string(), cik);
                            row.primary_cik = Some(path);
                        }
                        row.context
                            .insert("entity_role".to_string(), name.to_string());
                    } else if table == 4 {
                        for key in ["series-id", "owner-cik"] {
                            row.context.remove(&key.replace('-', "_"));
                            if let Some(v) = named(value, key).and_then(scalar) {
                                row.context.insert(key.replace('-', "_"), v);
                            }
                        }
                    }
                    context = row.context.clone();
                    index = Some(self.rows.len());
                    self.rows.push(row);
                    relative.clear();
                    original.clear();
                } else if array_item {
                    index = None;
                    relative.clear();
                    original.clear();
                }
                for (key, child) in object {
                    if [
                        "<sec-document>",
                        "<sec-header>",
                        "<ims-document>",
                        "<ims-header>",
                    ]
                    .iter()
                    .any(|prefix| key.starts_with(prefix))
                    {
                        continue;
                    }
                    if index.is_some_and(|i| self.rows[i].table == 1)
                        && ["secsgml_start_byte", "secsgml_end_byte"]
                            .contains(&key.to_lowercase().replace('-', "_").as_str())
                    {
                        continue;
                    }
                    let key_name = canonical(key);
                    let mut next = relative.clone();
                    next.push(key_name.clone());
                    let mut source = original.clone();
                    source.push(key.clone());
                    self.visit(
                        child,
                        index,
                        next,
                        context.clone(),
                        &key_name,
                        false,
                        source,
                    );
                }
            }
            Value::Array(array) => {
                let primitive: Vec<Option<String>> = array
                    .iter()
                    .filter(|v| !v.is_object() && !v.is_array())
                    .map(scalar)
                    .collect();
                if array.is_empty() || primitive.len() == array.len() || !primitive.is_empty() {
                    self.store(
                        index,
                        relative.clone(),
                        &original,
                        Some(MetadataValue::List(primitive)),
                    );
                }
                for (position, child) in array
                    .iter()
                    .enumerate()
                    .filter(|(_, v)| v.is_object() || v.is_array())
                {
                    let mut next = relative.clone();
                    next.push(position.to_string());
                    let mut source = original.clone();
                    source.push(position.to_string());
                    self.visit(child, index, next, context.clone(), name, true, source);
                }
            }
            _ => {
                if index.is_some_and(|i| self.rows[i].table == 0)
                    && (relative == ["accession-number"]
                        || relative == ["paper", "accession-number"])
                    && accession_value(value) == Some(self.accession)
                {
                    return;
                }
                self.store(
                    index,
                    relative,
                    &original,
                    scalar(value).map(MetadataValue::String),
                );
            }
        }
    }
}

/// Flatten standardized metadata. The caller's accession is authoritative.
/// Uses historically selected business fields; drops offsets, header tags and anonymous records.
/// All metadata values remain strings/string lists; only accessionNumber is an integer.
pub fn flatten_metadata(metadata: &Value, accession: u64) -> Result<Vec<MetadataTable>> {
    if !metadata.is_object() {
        bail!("filing metadata must be a JSON object");
    }
    if accession >= 1_000_000_000_000_000_000 {
        bail!("accession must fit 18 digits");
    }
    let mut projection = Projection {
        accession,
        root_keys: metadata
            .as_object()
            .unwrap()
            .keys()
            .map(|k| canonical(k))
            .collect(),
        rows: vec![RawRow::default()],
    };
    projection.visit(
        metadata,
        Some(0),
        Vec::new(),
        BTreeMap::new(),
        "",
        false,
        Vec::new(),
    );
    let mut tables: Vec<MetadataTable> = TABLES
        .iter()
        .map(|name| MetadataTable {
            name: name.to_string(),
            rows: Vec::new(),
        })
        .collect();
    for raw in projection.rows {
        let mut values = BTreeMap::new();
        for (key, value) in raw.fields {
            if let (Some(column), Some(value)) = (lookup()[TABLES[raw.table]].get(&key), value) {
                values.insert(column.clone(), value);
            }
        }
        // Inherited identifiers alone don't make a data row.
        if values.is_empty() {
            continue;
        }
        for key in match raw.table {
            2 | 3 => &["entity_cik", "entity_role"][..],
            5 => &["series_id", "owner_cik"][..],
            _ => &[][..],
        } {
            if let Some(value) = raw.context.get(*key) {
                values
                    .entry(key.to_string())
                    .or_insert_with(|| MetadataValue::String(value.clone()));
            }
        }
        tables[raw.table].rows.push(MetadataRow {
            accession_number: accession,
            values,
        });
    }
    Ok(tables)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn links_roles_lists_and_documents() {
        let metadata = json!({"accession-number":"0001234567-26-000001", "type":"8-K", "items":["1","1"], "filer":{"company-data":{"cik":"0001","conformed-name":"A","business-address":{"city":"B"}},"former-company":{"former-conformed-name":"Old"}}, "series":{"series-id":"S1","owner-cik":"0001","class-contract":[{"class-contract-id":"C1"},{}]}, "documents":[{"type":"8-K","filename":"main.htm","secsgml_start_byte":"512"},{}]});
        let tables = flatten_metadata(&metadata, 123456726000001).unwrap();
        assert_eq!(tables[1].rows.len(), 1);
        assert!(!tables[1].rows[0].values.contains_key("secsgml_start_byte"));
        assert_eq!(
            tables[3].rows[0].values["entity_cik"],
            MetadataValue::String("0001".into())
        );
        assert_eq!(
            tables[5].rows[0].values["series_id"],
            MetadataValue::String("S1".into())
        );
        assert_eq!(
            tables[2].rows[0].values["business_address__city"],
            MetadataValue::String("B".into())
        );
        assert!(!tables[0].rows[0].values.contains_key("reported_accession"));
        assert_eq!(
            tables[0]
                .to_record_batch()
                .unwrap()
                .schema()
                .field(0)
                .data_type(),
            &DataType::UInt64
        );
        assert!(matches!(
            tables[0]
                .to_record_batch()
                .unwrap()
                .schema()
                .field_with_name("items__list")
                .unwrap()
                .data_type(),
            DataType::List(_)
        ));
    }
    #[test]
    fn alias_conflicts_empty_values_and_reported_accession() {
        let metadata = json!({"accession-number":"0001234567-26-000002", "type":"", "filer":{"company-data":{"cik":"001","fiscal-year-end":"1231","assigned-sic":{"fiscal-year-end":"0630"}},"business-address":{"city":"A"},"business_address":{"city":"A"}},"items":[],"documents":[{"type":null}]});
        let tables = flatten_metadata(&metadata, 123456726000001).unwrap();
        assert_eq!(tables[1].rows.len(), 0);
        assert_eq!(
            tables[2].rows[0].values["company_data__fiscal_year_end__list"],
            MetadataValue::List(vec![Some("1231".into()), Some("0630".into())])
        );
        assert_eq!(
            tables[2].rows[0].values["business_address__city__list"],
            MetadataValue::List(vec![Some("A".into()), Some("A".into())])
        );
        assert_eq!(
            tables[0].rows[0].values["items__list"],
            MetadataValue::List(vec![])
        );
        assert!(tables[0].rows[0].values.contains_key("reported_accession"));
        assert!(tables[2].to_record_batch().is_ok());
    }
}
