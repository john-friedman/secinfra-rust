mod mapping;
mod parser;
mod registry;
mod types;

use anyhow::{anyhow, Result};

pub use mapping::{
    parse_mapping_json, StructuredXmlTableSpec, XmlMappingDocument, XmlPathList, XmlTableSpec,
};
pub use registry::{mapping_json_for_document_type, supported_xml_document_types};
pub use types::{XmlRow, XmlTable, XmlTables};

pub fn parse_xml_tables(bytes: &[u8], document_type: &str) -> Result<XmlTables> {
    let mapping_json = mapping_json_for_document_type(document_type)
        .ok_or_else(|| anyhow!("unsupported SEC XML document type: {document_type}"))?;
    parse_xml_tables_with_mapping(bytes, document_type, mapping_json)
}

pub fn parse_xml_tables_with_mapping(
    bytes: &[u8],
    document_type: &str,
    mapping_json: &str,
) -> Result<XmlTables> {
    let mapping = parse_mapping_json(mapping_json)?;
    parser::parse_xml_tables_mapped(bytes, document_type, &mapping)
}
