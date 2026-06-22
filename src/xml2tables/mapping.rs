use anyhow::{Context, Result};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

pub type XmlMappingDocument = IndexMap<String, XmlTableSpec>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum XmlPathList {
    One(String),
    Many(Vec<String>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum XmlTableSpec {
    Structured(StructuredXmlTableSpec),
    Legacy(IndexMap<String, String>),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StructuredXmlTableSpec {
    #[serde(default)]
    pub columns: IndexMap<String, String>,
    #[serde(default)]
    pub carry: IndexMap<String, String>,
    #[serde(default, alias = "rowPath")]
    pub row_path: Option<XmlPathList>,
    #[serde(default, alias = "contextPath")]
    pub context_path: Option<XmlPathList>,
    #[serde(default, alias = "rowIndex")]
    pub row_index: Option<String>,
    #[serde(default, alias = "contextIndex")]
    pub context_index: Option<String>,
}

pub fn parse_mapping_json(json: &str) -> Result<XmlMappingDocument> {
    let json = json.trim_start_matches('\u{feff}');
    serde_json::from_str(json).context("failed to parse SEC XML table mapping JSON")
}
