use anyhow::Result;
use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::QName;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::io::Cursor;

pub type XbrlMap = BTreeMap<String, String>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum XbrlFileType {
    Inline,
    ExtractedInline,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XbrlFact {
    #[serde(rename = "_val", skip_serializing_if = "Option::is_none")]
    pub val: Option<String>,
    #[serde(rename = "_attributes")]
    pub attributes: XbrlMap,
    #[serde(rename = "_context", skip_serializing_if = "Option::is_none")]
    pub context: Option<XbrlMap>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SimpleXbrlRecord {
    #[serde(rename = "accessionNumber")]
    pub accession_number: u64,
    pub context_id: Option<u64>,
    pub taxonomy: String,
    pub name: String,
    pub value: Option<String>,
    pub period_start_date: Option<String>,
    pub period_end_date: Option<String>,
    pub members: Option<String>,
}

pub fn parse_xbrl(bytes: &[u8], file_type: XbrlFileType) -> Result<Vec<XbrlFact>> {
    match file_type {
        XbrlFileType::Inline => parse_inline_xbrl(bytes),
        XbrlFileType::ExtractedInline => parse_extracted_inline_xbrl(bytes),
    }
}

pub fn parse_inline_xbrl(bytes: &[u8]) -> Result<Vec<XbrlFact>> {
    let contexts = parse_contexts(bytes)?;
    parse_facts(bytes, &contexts, FactMode::Inline)
}

pub fn parse_extracted_inline_xbrl(bytes: &[u8]) -> Result<Vec<XbrlFact>> {
    let contexts = parse_contexts(bytes)?;
    parse_facts(bytes, &contexts, FactMode::ExtractedInline)
}

pub fn construct_simple_xbrl(accession_number: u64, facts: &[XbrlFact]) -> Vec<SimpleXbrlRecord> {
    let mut records = Vec::new();
    let mut context_mapping = BTreeMap::new();
    let mut next_context_id = 0_u64;

    for fact in facts {
        let Some(full_name) = fact.attributes.get("name") else {
            continue;
        };
        let Some((taxonomy, name)) = full_name.split_once(':') else {
            continue;
        };
        if taxonomy.is_empty() || name.is_empty() {
            continue;
        }

        let context_ref = fact
            .context
            .as_ref()
            .and_then(|context| context.get("_contextref"));
        let context_id = context_ref.map(|context_ref| {
            *context_mapping
                .entry(context_ref.clone())
                .or_insert_with(|| {
                    let context_id = next_context_id;
                    next_context_id += 1;
                    context_id
                })
        });

        let value = scaled_value(fact.val.as_deref(), fact.attributes.get("scale"));
        let period_start_date = fact.context.as_ref().and_then(|context| {
            context
                .get("period_instant")
                .or_else(|| context.get("period_startdate"))
                .cloned()
        });
        let period_end_date = fact
            .context
            .as_ref()
            .and_then(|context| context.get("period_enddate"))
            .cloned();
        let members = fact.context.as_ref().and_then(context_members);

        records.push(SimpleXbrlRecord {
            accession_number,
            context_id,
            taxonomy: taxonomy.to_string(),
            name: name.to_string(),
            value,
            period_start_date,
            period_end_date,
            members,
        });
    }

    records
}

#[derive(Clone, Copy)]
enum FactMode {
    Inline,
    ExtractedInline,
}

struct FactBuilder {
    val: String,
    attributes: XbrlMap,
    context_ref: Option<String>,
    depth: usize,
}

struct ContextBuilder {
    id: Option<String>,
    data: XbrlMap,
    text_key: Option<String>,
    depth: usize,
}

fn parse_facts(
    bytes: &[u8],
    contexts: &BTreeMap<String, XbrlMap>,
    mode: FactMode,
) -> Result<Vec<XbrlFact>> {
    let mut reader = xml_reader(bytes);
    let mut buf = Vec::new();
    let mut facts = Vec::new();
    let mut current: Option<FactBuilder> = None;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                if let Some(fact) = current.as_mut() {
                    fact.depth += 1;
                } else if let Some(fact) = start_fact(&reader, &e, mode)? {
                    current = Some(fact);
                }
            }
            Event::Empty(e) => {
                if current.is_none() {
                    if let Some(fact) = start_fact(&reader, &e, mode)? {
                        facts.push(finish_fact(fact, contexts));
                    }
                }
            }
            Event::Text(e) => {
                if let Some(fact) = current.as_mut() {
                    fact.val.push_str(&e.unescape()?);
                }
            }
            Event::CData(e) => {
                if let Some(fact) = current.as_mut() {
                    fact.val.push_str(&String::from_utf8_lossy(e.as_ref()));
                }
            }
            Event::End(_) => {
                if let Some(fact) = current.as_mut() {
                    if fact.depth > 1 {
                        fact.depth -= 1;
                    } else if let Some(fact) = current.take() {
                        facts.push(finish_fact(fact, contexts));
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }

        buf.clear();
    }

    Ok(facts)
}

fn parse_contexts(bytes: &[u8]) -> Result<BTreeMap<String, XbrlMap>> {
    let mut reader = xml_reader(bytes);
    let mut buf = Vec::new();
    let mut contexts = BTreeMap::new();
    let mut current: Option<ContextBuilder> = None;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                if let Some(context) = current.as_mut() {
                    context.depth += 1;
                    context.text_key = context_text_key(&reader, &e)?;
                } else if is_local_name(e.name(), "context") {
                    let attributes = attributes_to_map(&reader, &e)?;
                    let id = attributes
                        .get("id")
                        .or_else(|| attributes.get("xml:id"))
                        .cloned();
                    let mut data = XbrlMap::new();
                    if let Some(id) = &id {
                        data.insert("_contextref".to_string(), id.clone());
                    }
                    current = Some(ContextBuilder {
                        id,
                        data,
                        text_key: None,
                        depth: 1,
                    });
                }
            }
            Event::Empty(e) => {
                if let Some(context) = current.as_mut() {
                    if let Some((key, value)) = empty_context_value(&reader, &e)? {
                        context.data.insert(key, value);
                    }
                }
            }
            Event::Text(e) => {
                if let Some(context) = current.as_mut() {
                    if let Some(key) = &context.text_key {
                        context
                            .data
                            .entry(key.clone())
                            .and_modify(|value| value.push_str(&e.unescape().unwrap_or_default()))
                            .or_insert_with(|| e.unescape().unwrap_or_default().into_owned());
                    }
                }
            }
            Event::CData(e) => {
                if let Some(context) = current.as_mut() {
                    if let Some(key) = &context.text_key {
                        context
                            .data
                            .entry(key.clone())
                            .and_modify(|value| {
                                value.push_str(&String::from_utf8_lossy(e.as_ref()))
                            })
                            .or_insert_with(|| String::from_utf8_lossy(e.as_ref()).into_owned());
                    }
                }
            }
            Event::End(e) => {
                if let Some(context) = current.as_mut() {
                    context.text_key = None;
                    if context.depth > 1 {
                        context.depth -= 1;
                    } else if is_local_name(e.name(), "context") {
                        if let Some(context) = current.take() {
                            if let Some(id) = context.id {
                                contexts.insert(id, context.data);
                            }
                        }
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }

        buf.clear();
    }

    Ok(contexts)
}

fn start_fact<R: std::io::BufRead>(
    reader: &Reader<R>,
    start: &BytesStart<'_>,
    mode: FactMode,
) -> Result<Option<FactBuilder>> {
    if matches!(mode, FactMode::Inline) && !is_inline_fact_name(start.name()) {
        return Ok(None);
    }

    let mut attributes = attributes_to_map(reader, start)?;
    if matches!(mode, FactMode::ExtractedInline) {
        if !attributes.contains_key("contextRef") {
            return Ok(None);
        }
        attributes
            .entry("name".to_string())
            .or_insert_with(|| qname_to_string(start.name()));
    }

    let context_ref = attributes.get("contextRef").cloned();

    Ok(Some(FactBuilder {
        val: String::new(),
        attributes,
        context_ref,
        depth: 1,
    }))
}

fn finish_fact(fact: FactBuilder, contexts: &BTreeMap<String, XbrlMap>) -> XbrlFact {
    let val = if fact.val.trim().is_empty() {
        None
    } else {
        Some(fact.val.trim().to_string())
    };
    let context = fact
        .context_ref
        .as_ref()
        .and_then(|context_ref| contexts.get(context_ref))
        .cloned();

    XbrlFact {
        val,
        attributes: fact.attributes,
        context,
    }
}

fn scaled_value(value: Option<&str>, scale: Option<&String>) -> Option<String> {
    let value = value?;
    let Some(scale) = scale else {
        return Some(value.to_string());
    };
    let Ok(scale) = scale.parse::<i32>() else {
        return Some(value.to_string());
    };

    Some(apply_decimal_scale(value, scale).unwrap_or_else(|| value.to_string()))
}

fn apply_decimal_scale(value: &str, scale: i32) -> Option<String> {
    let value = value.trim();
    let negative = value.starts_with('-');
    let unsigned = value
        .strip_prefix('-')
        .or_else(|| value.strip_prefix('+'))
        .unwrap_or(value);
    let unsigned = unsigned.replace(',', "");

    if unsigned.is_empty()
        || unsigned
            .chars()
            .any(|ch| !ch.is_ascii_digit() && ch != '.')
        || unsigned.matches('.').count() > 1
    {
        return None;
    }

    let (integer, fractional) = unsigned.split_once('.').unwrap_or((&unsigned, ""));
    let digits = format!("{integer}{fractional}");
    let digits = digits.trim_start_matches('0');
    let digits = if digits.is_empty() { "0" } else { digits };
    let decimal_places = fractional.len() as i32 - scale;

    let mut scaled = match decimal_places.cmp(&0) {
        Ordering::Less | Ordering::Equal => {
            let zeros = "0".repeat(decimal_places.unsigned_abs() as usize);
            format!("{digits}{zeros}")
        }
        Ordering::Greater => {
            let decimal_places = decimal_places as usize;
            if digits.len() <= decimal_places {
                let zeros = "0".repeat(decimal_places - digits.len());
                format!("0.{zeros}{digits}")
            } else {
                let split_at = digits.len() - decimal_places;
                format!("{}.{}", &digits[..split_at], &digits[split_at..])
            }
        }
    };

    if let Some((integer, fractional)) = scaled.split_once('.') {
        let integer = integer.trim_start_matches('0');
        let integer = if integer.is_empty() { "0" } else { integer };
        scaled = format!("{integer}.{fractional}");
    } else {
        let normalized = scaled.trim_start_matches('0');
        scaled = if normalized.is_empty() {
            "0".to_string()
        } else {
            normalized.to_string()
        };
    }

    if negative && scaled != "0" {
        scaled.insert(0, '-');
    }

    Some(scaled)
}

fn context_members(context: &XbrlMap) -> Option<String> {
    if let Some(members) = context
        .get("entity_segment_explicitmember")
        .or_else(|| context.get("explicitmember"))
        .filter(|members| !members.is_empty())
    {
        return Some(members.clone());
    }

    let members = context
        .iter()
        .filter_map(|(key, value)| {
            if key.starts_with("dimension:") && !value.is_empty() {
                Some(value.as_str())
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    if members.is_empty() {
        None
    } else {
        Some(members.join(","))
    }
}

fn is_inline_fact_name(name: QName<'_>) -> bool {
    is_local_name(name, "nonfraction") || is_local_name(name, "nonnumeric")
}

fn context_text_key<R: std::io::BufRead>(
    reader: &Reader<R>,
    start: &BytesStart<'_>,
) -> Result<Option<String>> {
    if is_local_name(start.name(), "identifier") {
        return Ok(Some("entity_identifier".to_string()));
    }
    if is_local_name(start.name(), "instant") {
        return Ok(Some("period_instant".to_string()));
    }
    if is_local_name(start.name(), "startdate") {
        return Ok(Some("period_startdate".to_string()));
    }
    if is_local_name(start.name(), "enddate") {
        return Ok(Some("period_enddate".to_string()));
    }
    if is_local_name(start.name(), "explicitmember") {
        let attributes = attributes_to_map(reader, start)?;
        if let Some(dimension) = attributes.get("dimension") {
            return Ok(Some(format!("dimension:{dimension}")));
        }
        return Ok(Some("explicitmember".to_string()));
    }

    Ok(None)
}

fn empty_context_value<R: std::io::BufRead>(
    reader: &Reader<R>,
    start: &BytesStart<'_>,
) -> Result<Option<(String, String)>> {
    if is_local_name(start.name(), "explicitmember") {
        let attributes = attributes_to_map(reader, start)?;
        if let Some(dimension) = attributes.get("dimension") {
            return Ok(Some((format!("dimension:{dimension}"), String::new())));
        }
    }

    Ok(None)
}

fn attributes_to_map<R: std::io::BufRead>(
    reader: &Reader<R>,
    start: &BytesStart<'_>,
) -> Result<XbrlMap> {
    let mut attributes = XbrlMap::new();
    for attribute in start.attributes().with_checks(false) {
        let attribute = attribute?;
        let key = qname_to_string(attribute.key);
        let value = attribute
            .decode_and_unescape_value(reader.decoder())?
            .into_owned();
        attributes.insert(key, value);
    }
    Ok(attributes)
}

fn xml_reader(bytes: &[u8]) -> Reader<Cursor<&[u8]>> {
    let mut reader = Reader::from_reader(Cursor::new(bytes));
    reader.config_mut().trim_text(true);
    reader
}

fn qname_to_string(name: QName<'_>) -> String {
    String::from_utf8_lossy(name.into_inner()).into_owned()
}

fn is_local_name(name: QName<'_>, expected: &str) -> bool {
    name.local_name()
        .as_ref()
        .eq_ignore_ascii_case(expected.as_bytes())
}
