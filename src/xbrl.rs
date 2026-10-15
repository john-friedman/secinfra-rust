use anyhow::{Result, bail};
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::{QName, ResolveResult};
use quick_xml::{NsReader, Reader};
use scraper::{ElementRef, Html};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::io::Cursor;

pub type XbrlMap = BTreeMap<String, String>;

/// Context leaf paths follow secxbrl, including arrays of explicit members.
pub type XbrlContext = BTreeMap<String, XbrlContextValue>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum XbrlContextValue {
    String(String),
    List(Vec<String>),
    Dimensions(Vec<XbrlDimension>),
}

impl XbrlContextValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&[String]> {
        match self {
            Self::List(values) => Some(values),
            _ => None,
        }
    }

    pub fn as_dimensions(&self) -> Option<&[XbrlDimension]> {
        match self {
            Self::Dimensions(values) => Some(values),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum XbrlDimensionType {
    Explicit,
    Typed,
}

/// The axis and its member or typed text value, kept together in document order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XbrlDimension {
    pub dimension: String,
    #[serde(rename = "type")]
    pub dimension_type: XbrlDimensionType,
    pub value: String,
}

/// Resolved measures from the referenced unit, including divided units.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct XbrlUnit {
    pub numerator: Vec<String>,
    pub denominator: Vec<String>,
}

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
    pub context: Option<XbrlContext>,
    #[serde(rename = "_unit", default, skip_serializing_if = "Option::is_none")]
    pub unit: Option<XbrlUnit>,
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
    #[serde(default)]
    pub dimensions: Vec<XbrlDimension>,
    pub unit: Option<String>,
    pub decimals: Option<String>,
    pub precision: Option<String>,
}

pub fn parse_xbrl(bytes: &[u8], file_type: XbrlFileType) -> Result<Vec<XbrlFact>> {
    match file_type {
        XbrlFileType::Inline => parse_inline_xbrl(bytes),
        XbrlFileType::ExtractedInline => parse_extracted_inline_xbrl(bytes),
    }
}

pub fn parse_inline_xbrl(bytes: &[u8]) -> Result<Vec<XbrlFact>> {
    // Inline filings are HTML: accept void tags, HTML entities and lowercase attributes.
    let document = Html::parse_document(std::str::from_utf8(bytes)?);
    let units = inline_units(&document);
    let mut contexts = BTreeMap::new();
    for element in document
        .root_element()
        .descendants()
        .filter_map(ElementRef::wrap)
    {
        if local_html_name(element) != "context" {
            continue;
        }
        let Some(id) = element.attr("id").or_else(|| element.attr("xml:id")) else {
            continue;
        };
        let mut context = XbrlContext::new();
        context.insert("_contextref".into(), XbrlContextValue::String(id.into()));
        let mut path = Vec::new();
        for child in element.children().filter_map(ElementRef::wrap) {
            collect_inline_context(child, &mut path, &mut context);
        }
        contexts.insert(id.to_string(), context);
    }

    let mut facts = Vec::new();
    for element in document
        .root_element()
        .descendants()
        .filter_map(ElementRef::wrap)
    {
        if !matches!(element.value().name(), "ix:nonfraction" | "ix:nonnumeric") {
            continue;
        }
        let mut attributes: XbrlMap = element
            .value()
            .attrs()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect();
        let context = attributes
            .remove("contextref")
            .and_then(|context_ref| contexts.get(&context_ref).cloned());
        let unit = attributes
            .get("unitref")
            .and_then(|unit_ref| units.get(unit_ref))
            .cloned();
        let text = element
            .text()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        facts.push(XbrlFact {
            val: (!text.is_empty()).then_some(text),
            attributes,
            context,
            unit,
        });
    }
    Ok(facts)
}

pub fn parse_extracted_inline_xbrl(bytes: &[u8]) -> Result<Vec<XbrlFact>> {
    let contexts = parse_contexts(bytes)?;
    let units = parse_units(bytes)?;
    parse_facts(bytes, &contexts, &units)
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
            .and_then(|context| context.get("_contextref"))
            .and_then(XbrlContextValue::as_str);
        let context_id = context_ref.map(|context_ref| {
            *context_mapping
                .entry(context_ref.to_string())
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
                .and_then(XbrlContextValue::as_str)
                .map(str::to_string)
        });
        let period_end_date = fact
            .context
            .as_ref()
            .and_then(|context| context.get("period_enddate"))
            .and_then(XbrlContextValue::as_str)
            .map(str::to_string);
        let members = fact.context.as_ref().and_then(context_members);
        let dimensions = fact
            .context
            .as_ref()
            .map(context_dimensions)
            .unwrap_or_default();
        let unit = fact.unit.as_ref().and_then(simple_unit);
        let decimals = fact.attributes.get("decimals").cloned();
        let precision = fact.attributes.get("precision").cloned();

        records.push(SimpleXbrlRecord {
            accession_number,
            context_id,
            taxonomy: taxonomy.to_string(),
            name: name.to_string(),
            value,
            period_start_date,
            period_end_date,
            members,
            dimensions,
            unit,
            decimals,
            precision,
        });
    }

    records
}

struct FactBuilder {
    val: String,
    attributes: XbrlMap,
    context_ref: String,
    depth: usize,
}

struct ContextBuilder {
    id: Option<String>,
    data: XbrlContext,
    elements: Vec<ContextElement>,
}

struct ContextElement {
    name: String,
    text: String,
    has_children: bool,
    dimension: Option<XbrlDimension>,
}

fn parse_facts(
    bytes: &[u8],
    contexts: &BTreeMap<String, XbrlContext>,
    units: &BTreeMap<String, XbrlUnit>,
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
                } else if let Some(fact) = start_fact(&reader, &e)? {
                    current = Some(fact);
                }
            }
            Event::Empty(e) => {
                if current.is_none() {
                    if let Some(fact) = start_fact(&reader, &e)? {
                        facts.push(finish_fact(fact, contexts, units));
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
                        facts.push(finish_fact(fact, contexts, units));
                    }
                }
            }
            Event::Eof => {
                if current.is_some() {
                    bail!("unterminated XBRL fact");
                }
                break;
            }
            _ => {}
        }

        buf.clear();
    }

    Ok(facts)
}

fn parse_contexts(bytes: &[u8]) -> Result<BTreeMap<String, XbrlContext>> {
    let mut reader = xml_reader(bytes);
    let mut buf = Vec::new();
    let mut contexts = BTreeMap::new();
    let mut current: Option<ContextBuilder> = None;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                if let Some(context) = current.as_mut() {
                    if let Some(parent) = context.elements.last_mut() {
                        parent.has_children = true;
                    }
                    context.elements.push(ContextElement {
                        name: String::from_utf8_lossy(e.local_name().as_ref()).to_lowercase(),
                        text: String::new(),
                        has_children: false,
                        dimension: xml_dimension(&reader, &e)?,
                    });
                } else if is_local_name(e.name(), "context") {
                    current = Some(start_context(&reader, &e)?);
                }
            }
            Event::Empty(e) => {
                if let Some(context) = current.as_mut() {
                    if let Some(parent) = context.elements.last_mut() {
                        parent.has_children = true;
                    }
                    if let Some(dimension) = xml_dimension(&reader, &e)? {
                        insert_dimension(&mut context.data, dimension);
                    }
                } else if is_local_name(e.name(), "context") {
                    let context = start_context(&reader, &e)?;
                    if let Some(id) = context.id {
                        contexts.insert(id, context.data);
                    }
                }
            }
            Event::Text(e) => {
                if let Some(context) = current.as_mut() {
                    let text = e.unescape()?;
                    if let Some(element) = context.elements.last_mut() {
                        element.text.push_str(&text);
                    }
                    for element in &mut context.elements {
                        if let Some(dimension) = &mut element.dimension {
                            dimension.value.push_str(&text);
                        }
                    }
                }
            }
            Event::CData(e) => {
                if let Some(context) = current.as_mut() {
                    let text = String::from_utf8_lossy(e.as_ref());
                    if let Some(element) = context.elements.last_mut() {
                        element.text.push_str(&text);
                    }
                    for element in &mut context.elements {
                        if let Some(dimension) = &mut element.dimension {
                            dimension.value.push_str(&text);
                        }
                    }
                }
            }
            Event::End(e) => {
                if let Some(context) = current.as_mut() {
                    if let Some(element) = context.elements.pop() {
                        if let Some(mut dimension) = element.dimension {
                            dimension.value = dimension.value.trim().to_string();
                            insert_dimension(&mut context.data, dimension);
                        }
                        if !element.has_children {
                            let key = context
                                .elements
                                .iter()
                                .map(|parent| parent.name.as_str())
                                .chain(std::iter::once(element.name.as_str()))
                                .collect::<Vec<_>>()
                                .join("_");
                            insert_context_leaf(
                                &mut context.data,
                                key,
                                &element.name,
                                &element.text,
                            );
                        }
                    } else if is_local_name(e.name(), "context") {
                        if let Some(context) = current.take() {
                            if let Some(id) = context.id {
                                contexts.insert(id, context.data);
                            }
                        }
                    }
                }
            }
            Event::Eof => {
                if current.is_some() {
                    bail!("unterminated XBRL context");
                }
                break;
            }
            _ => {}
        }

        buf.clear();
    }

    Ok(contexts)
}

fn start_context<R: std::io::BufRead>(
    reader: &Reader<R>,
    start: &BytesStart<'_>,
) -> Result<ContextBuilder> {
    let attributes = attributes_to_map(reader, start)?;
    let id = attributes
        .get("id")
        .or_else(|| attributes.get("xml:id"))
        .cloned();
    let mut data = XbrlContext::new();
    if let Some(id) = &id {
        data.insert(
            "_contextref".to_string(),
            XbrlContextValue::String(id.clone()),
        );
    }
    Ok(ContextBuilder {
        id,
        data,
        elements: Vec::new(),
    })
}

fn start_fact<R: std::io::BufRead>(
    reader: &Reader<R>,
    start: &BytesStart<'_>,
) -> Result<Option<FactBuilder>> {
    let mut attributes = attributes_to_map(reader, start)?;
    let Some(context_ref) = attributes.remove("contextRef") else {
        return Ok(None);
    };
    attributes
        .entry("name".to_string())
        .or_insert_with(|| qname_to_string(start.name()));

    Ok(Some(FactBuilder {
        val: String::new(),
        attributes,
        context_ref,
        depth: 1,
    }))
}

fn finish_fact(
    fact: FactBuilder,
    contexts: &BTreeMap<String, XbrlContext>,
    units: &BTreeMap<String, XbrlUnit>,
) -> XbrlFact {
    // Extracted text blocks contain escaped HTML; expose their text as secxbrl does.
    let text = if fact.val.contains(['<', '&']) {
        Html::parse_fragment(&fact.val)
            .root_element()
            .text()
            .collect::<String>()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    } else {
        fact.val.split_whitespace().collect::<Vec<_>>().join(" ")
    };
    let val = (!text.is_empty()).then_some(text);
    let context = contexts.get(&fact.context_ref).cloned();
    let unit = fact
        .attributes
        .get("unitRef")
        .and_then(|unit_ref| units.get(unit_ref))
        .cloned();

    XbrlFact {
        val,
        attributes: fact.attributes,
        context,
        unit,
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
        || unsigned.chars().any(|ch| !ch.is_ascii_digit() && ch != '.')
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

fn context_members(context: &XbrlContext) -> Option<String> {
    let mut members = Vec::new();
    for (key, value) in context {
        if key == "explicitmember" || key.ends_with("_explicitmember") {
            match value {
                XbrlContextValue::String(value) => members.push(value.as_str()),
                XbrlContextValue::List(values) => members.extend(values.iter().map(String::as_str)),
                XbrlContextValue::Dimensions(_) => {}
            }
        }
    }
    // Previously archived Rust facts used dimension:<axis> string entries.
    if members.iter().all(|value| value.is_empty()) {
        members = context
            .iter()
            .filter_map(|(key, value)| {
                key.starts_with("dimension:")
                    .then(|| value.as_str())
                    .flatten()
            })
            .collect();
    }
    members.retain(|value| !value.is_empty());
    (!members.is_empty()).then(|| members.join(","))
}

fn context_dimensions(context: &XbrlContext) -> Vec<XbrlDimension> {
    if let Some(dimensions) = context
        .get("_dimensions")
        .and_then(XbrlContextValue::as_dimensions)
    {
        return dimensions.to_vec();
    }
    context
        .iter()
        .filter_map(|(key, value)| {
            Some(XbrlDimension {
                dimension: key.strip_prefix("dimension:")?.to_string(),
                dimension_type: XbrlDimensionType::Explicit,
                value: value.as_str()?.to_string(),
            })
        })
        .collect()
}

fn xml_dimension<R: std::io::BufRead>(
    reader: &Reader<R>,
    start: &BytesStart<'_>,
) -> Result<Option<XbrlDimension>> {
    let dimension_type = if is_local_name(start.name(), "explicitmember") {
        XbrlDimensionType::Explicit
    } else if is_local_name(start.name(), "typedmember") {
        XbrlDimensionType::Typed
    } else {
        return Ok(None);
    };
    Ok(attributes_to_map(reader, start)?
        .remove("dimension")
        .map(|dimension| XbrlDimension {
            dimension,
            dimension_type,
            value: String::new(),
        }))
}

fn insert_dimension(context: &mut XbrlContext, dimension: XbrlDimension) {
    let dimensions = context
        .entry("_dimensions".to_string())
        .or_insert_with(|| XbrlContextValue::Dimensions(Vec::new()));
    if let XbrlContextValue::Dimensions(dimensions) = dimensions {
        dimensions.push(dimension);
    }
}

struct UnitBuilder {
    id: String,
    unit: XbrlUnit,
    elements: Vec<String>,
    measure: String,
}

fn parse_units(bytes: &[u8]) -> Result<BTreeMap<String, XbrlUnit>> {
    let mut reader = NsReader::from_reader(Cursor::new(bytes));
    let mut buf = Vec::new();
    let mut units = BTreeMap::new();
    let mut current: Option<UnitBuilder> = None;
    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                if let Some(unit) = current.as_mut() {
                    let name = String::from_utf8_lossy(e.local_name().as_ref()).to_lowercase();
                    if name == "measure" {
                        unit.measure.clear();
                    }
                    unit.elements.push(name);
                } else if is_local_name(e.name(), "unit") {
                    if let Some(id) = attributes_to_map(&reader, &e)?.remove("id") {
                        current = Some(UnitBuilder {
                            id,
                            unit: XbrlUnit {
                                numerator: Vec::new(),
                                denominator: Vec::new(),
                            },
                            elements: Vec::new(),
                            measure: String::new(),
                        });
                    }
                }
            }
            Event::Text(e) => {
                if let Some(unit) = current.as_mut() {
                    if unit.elements.last().is_some_and(|name| name == "measure") {
                        unit.measure.push_str(&e.unescape()?);
                    }
                }
            }
            Event::CData(e) => {
                if let Some(unit) = current.as_mut() {
                    if unit.elements.last().is_some_and(|name| name == "measure") {
                        unit.measure.push_str(&String::from_utf8_lossy(e.as_ref()));
                    }
                }
            }
            Event::End(e) => {
                if let Some(unit) = current.as_mut() {
                    if let Some(name) = unit.elements.pop() {
                        if name == "measure" && !unit.measure.trim().is_empty() {
                            let lexical = unit.measure.trim();
                            let (namespace, local_name) =
                                reader.resolve_element(QName(lexical.as_bytes()));
                            let namespace = match namespace {
                                ResolveResult::Bound(namespace) => {
                                    Some(String::from_utf8_lossy(namespace.as_ref()).into_owned())
                                }
                                _ => None,
                            };
                            let measure = unit_measure(
                                namespace.as_deref(),
                                &String::from_utf8_lossy(local_name.as_ref()),
                                lexical,
                            );
                            if unit.elements.iter().any(|name| name == "unitdenominator") {
                                unit.unit.denominator.push(measure);
                            } else {
                                unit.unit.numerator.push(measure);
                            }
                        }
                    } else if is_local_name(e.name(), "unit") {
                        if let Some(unit) = current.take() {
                            units.insert(unit.id, unit.unit);
                        }
                    }
                }
            }
            Event::Eof => {
                if current.is_some() {
                    bail!("unterminated XBRL unit");
                }
                break;
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(units)
}

fn inline_units(document: &Html) -> BTreeMap<String, XbrlUnit> {
    let mut units = BTreeMap::new();
    for element in document
        .root_element()
        .descendants()
        .filter_map(ElementRef::wrap)
    {
        if local_html_name(element) != "unit" {
            continue;
        }
        let Some(id) = element.attr("id") else {
            continue;
        };
        let mut unit = XbrlUnit {
            numerator: Vec::new(),
            denominator: Vec::new(),
        };
        for measure in element.descendants().filter_map(ElementRef::wrap) {
            if local_html_name(measure) != "measure" {
                continue;
            }
            let text = measure.text().collect::<String>();
            let lexical = text.trim();
            if lexical.is_empty() {
                continue;
            }
            let (namespace_key, local_name) = match lexical.split_once(':') {
                Some((prefix, name)) => (format!("xmlns:{prefix}"), name),
                None => ("xmlns".to_string(), lexical),
            };
            let namespace = std::iter::once(measure)
                .chain(measure.ancestors().filter_map(ElementRef::wrap))
                .find_map(|element| element.attr(namespace_key.as_str()));
            let value = unit_measure(namespace, local_name, lexical);
            if measure
                .ancestors()
                .filter_map(ElementRef::wrap)
                .take_while(|parent| local_html_name(*parent) != "unit")
                .any(|parent| local_html_name(parent) == "unitdenominator")
            {
                unit.denominator.push(value);
            } else {
                unit.numerator.push(value);
            }
        }
        units.insert(id.to_string(), unit);
    }
    units
}

fn unit_measure(namespace: Option<&str>, name: &str, lexical: &str) -> String {
    match namespace {
        Some("http://www.xbrl.org/2003/iso4217" | "http://www.xbrl.org/2003/instance") => {
            name.to_string()
        }
        Some(namespace) => format!("{{{namespace}}}{name}"),
        None => lexical.to_string(),
    }
}

fn simple_unit(unit: &XbrlUnit) -> Option<String> {
    if unit.numerator.is_empty() {
        return None;
    }
    let mut numerator = unit.numerator.clone();
    let mut denominator = unit.denominator.clone();
    numerator.sort();
    denominator.sort();
    let numerator = numerator.join("*");
    Some(match denominator.len() {
        0 => numerator,
        1 => format!("{numerator}/{}", denominator[0]),
        _ => format!("{numerator}/({})", denominator.join("*")),
    })
}

fn local_html_name(element: ElementRef<'_>) -> &str {
    element.value().name().rsplit(':').next().unwrap_or("")
}

fn collect_inline_context(
    element: ElementRef<'_>,
    path: &mut Vec<String>,
    context: &mut XbrlContext,
) {
    let name = local_html_name(element);
    if matches!(name, "explicitmember" | "typedmember") {
        if let Some(axis) = element.attr("dimension") {
            insert_dimension(
                context,
                XbrlDimension {
                    dimension: axis.to_string(),
                    dimension_type: if name == "typedmember" {
                        XbrlDimensionType::Typed
                    } else {
                        XbrlDimensionType::Explicit
                    },
                    value: element.text().collect::<String>().trim().to_string(),
                },
            );
        }
    }
    path.push(name.to_string());
    let mut has_children = false;
    for child in element.children().filter_map(ElementRef::wrap) {
        has_children = true;
        collect_inline_context(child, path, context);
    }
    if !has_children {
        let text = element.text().collect::<String>();
        insert_context_leaf(context, path.join("_"), name, &text);
    }
    path.pop();
}

fn insert_context_leaf(context: &mut XbrlContext, key: String, name: &str, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    if name == "explicitmember" {
        let value = context
            .entry(key)
            .or_insert_with(|| XbrlContextValue::List(Vec::new()));
        if let XbrlContextValue::List(values) = value {
            values.push(text.to_string());
        }
    } else {
        context.insert(key, XbrlContextValue::String(text.to_string()));
    }
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
    // Trim complete leaf values, not individual chunks separated by CDATA or tags.
    Reader::from_reader(Cursor::new(bytes))
}

fn qname_to_string(name: QName<'_>) -> String {
    String::from_utf8_lossy(name.into_inner()).into_owned()
}

fn is_local_name(name: QName<'_>, expected: &str) -> bool {
    name.local_name()
        .as_ref()
        .eq_ignore_ascii_case(expected.as_bytes())
}
