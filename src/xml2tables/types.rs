use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

pub type XmlRow = IndexMap<String, Option<String>>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XmlTables {
    pub document_type: String,
    pub tables: Vec<XmlTable>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XmlTable {
    pub name: String,
    pub columns: Vec<String>,
    pub rows: Vec<XmlRow>,
}
