use crate::xml2tables::{
    XmlMappingDocument, XmlPathList, XmlRow, XmlTable, XmlTableSpec, XmlTables,
};
use anyhow::Result;
use indexmap::{IndexMap, IndexSet};
use quick_xml::events::{BytesStart, Event};
use quick_xml::name::QName;
use quick_xml::Reader;
use std::collections::{HashMap, HashSet};
use std::io::Cursor;

pub(crate) fn parse_xml_tables_mapped(
    bytes: &[u8],
    document_type: &str,
    mapping: &XmlMappingDocument,
) -> Result<XmlTables> {
    let real_paths = discover_real_paths(bytes)?;
    let specs = normalize_table_specs(mapping);
    let specs = resolve_table_specs(specs, &real_paths);
    let tables = parse_resolved_tables(bytes, specs)?;

    Ok(XmlTables {
        document_type: document_type.to_string(),
        tables,
    })
}

#[derive(Debug, Clone)]
struct NormalizedTableSpec {
    name: String,
    columns: IndexMap<String, String>,
    carry: IndexMap<String, String>,
    row_paths: Vec<String>,
    context_paths: Vec<String>,
    row_index_col: Option<String>,
    context_index_col: Option<String>,
    explicit_row_path: bool,
}

#[derive(Debug, Clone)]
struct ResolvedTableSpec {
    name: String,
    columns: IndexMap<String, String>,
    carry: IndexMap<String, String>,
    row_boundary: String,
    context_path: Option<String>,
    row_index_col: Option<String>,
    context_index_col: Option<String>,
    explicit_row_path: bool,
    output_columns: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ValueTarget {
    Column,
    GlobalCarry,
    ContextCarry,
}

#[derive(Debug, Clone)]
struct PathAction {
    table_id: usize,
    target: ValueTarget,
    column: String,
}

struct PathIndex {
    text_actions: HashMap<String, Vec<PathAction>>,
    attr_actions: HashMap<String, Vec<PathAction>>,
    row_boundaries: HashMap<String, Vec<usize>>,
    context_starts: HashMap<String, Vec<usize>>,
    context_ends: HashMap<String, Vec<usize>>,
}

struct TableState {
    spec: ResolvedTableSpec,
    row_values: IndexMap<String, Vec<String>>,
    global_carry: IndexMap<String, Vec<String>>,
    context_carry: IndexMap<String, Vec<String>>,
    rows: Vec<XmlRow>,
    open_context_rows: Vec<usize>,
    table_row_index: usize,
    context_index: usize,
    current_context_id: Option<usize>,
}

struct ElementFrame {
    tag: String,
    attributes: Vec<(String, String)>,
    text: String,
    has_child: bool,
}

impl TableState {
    fn new(spec: ResolvedTableSpec) -> Self {
        let row_values = accumulator_for(spec.columns.values());
        let global_carry = accumulator_for(spec.carry.iter().filter_map(|(path, column)| {
            if is_context_carry_path(&spec, path) {
                None
            } else {
                Some(column)
            }
        }));
        let context_carry = accumulator_for(spec.carry.iter().filter_map(|(path, column)| {
            if is_context_carry_path(&spec, path) {
                Some(column)
            } else {
                None
            }
        }));

        Self {
            spec,
            row_values,
            global_carry,
            context_carry,
            rows: Vec::new(),
            open_context_rows: Vec::new(),
            table_row_index: 0,
            context_index: 0,
            current_context_id: None,
        }
    }

    fn start_context(&mut self) {
        self.context_index += 1;
        self.current_context_id = Some(self.context_index);
        self.open_context_rows.clear();
        clear_accumulator(&mut self.context_carry);
    }

    fn push_value(&mut self, target: ValueTarget, column: &str, value: String) {
        match target {
            ValueTarget::Column => push_accumulator_value(&mut self.row_values, column, value),
            ValueTarget::GlobalCarry => {
                push_accumulator_value(&mut self.global_carry, column, value)
            }
            ValueTarget::ContextCarry => {
                push_accumulator_value(&mut self.context_carry, column, value)
            }
        }
    }

    fn emit_row(&mut self) {
        let mut row = XmlRow::new();

        for column in &self.spec.output_columns {
            row.insert(column.clone(), None);
        }

        apply_accumulator_to_row(&mut row, &self.row_values, false);
        apply_accumulator_to_row(&mut row, &self.global_carry, true);
        apply_accumulator_to_row(&mut row, &self.context_carry, true);

        if let Some(column) = &self.spec.row_index_col {
            self.table_row_index += 1;
            row.insert(column.clone(), Some(self.table_row_index.to_string()));
        }

        if let Some(column) = &self.spec.context_index_col {
            row.insert(
                column.clone(),
                self.current_context_id.map(|id| id.to_string()),
            );
        }

        row.insert("_table".to_string(), Some(self.spec.name.clone()));

        if row_has_non_meta_value(&row, &self.spec) {
            self.rows.push(row);
            if self.spec.context_path.is_some() {
                self.open_context_rows.push(self.rows.len() - 1);
            }
        }

        clear_accumulator(&mut self.row_values);
    }

    fn finalize_context_carry(&mut self) {
        for row_idx in &self.open_context_rows {
            if let Some(row) = self.rows.get_mut(*row_idx) {
                apply_accumulator_to_row(row, &self.context_carry, true);
            }
        }
        self.open_context_rows.clear();
        clear_accumulator(&mut self.context_carry);
        self.current_context_id = None;
    }

    fn finish(&mut self) {
        if !self.spec.explicit_row_path {
            self.emit_row();
        }

        if self.spec.context_path.is_some() && !self.open_context_rows.is_empty() {
            self.finalize_context_carry();
        }

        for row in &mut self.rows {
            apply_accumulator_to_row(row, &self.global_carry, true);
        }
    }

    fn into_table(self) -> XmlTable {
        XmlTable {
            name: self.spec.name,
            columns: self.spec.output_columns,
            rows: self.rows,
        }
    }
}

fn discover_real_paths(bytes: &[u8]) -> Result<HashSet<String>> {
    let mut reader = xml_reader(bytes);
    let mut buf = Vec::new();
    let mut real_paths = HashSet::new();
    let mut stack = Vec::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                stack.push(normalize_tag(e.name()));
                add_current_path(&reader, &e, &stack, &mut real_paths)?;
            }
            Event::Empty(e) => {
                stack.push(normalize_tag(e.name()));
                add_current_path(&reader, &e, &stack, &mut real_paths)?;
                stack.pop();
            }
            Event::End(_) => {
                if !stack.is_empty() {
                    let path = current_path(&stack);
                    real_paths.insert(path);
                    stack.pop();
                }
            }
            Event::Eof => break,
            _ => {}
        }

        buf.clear();
    }

    Ok(real_paths)
}

fn add_current_path<R: std::io::BufRead>(
    reader: &Reader<R>,
    start: &BytesStart<'_>,
    stack: &[String],
    real_paths: &mut HashSet<String>,
) -> Result<()> {
    let path = current_path(stack);
    real_paths.insert(path.clone());

    for attribute in start.attributes().with_checks(false) {
        let attribute = attribute?;
        real_paths.insert(format!("{path}/@{}", normalize_attr_name(attribute.key)));
        let _ = attribute.decode_and_unescape_value(reader.decoder())?;
    }

    Ok(())
}

fn normalize_table_specs(mapping: &XmlMappingDocument) -> Vec<NormalizedTableSpec> {
    mapping
        .iter()
        .map(|(name, spec)| match spec {
            XmlTableSpec::Legacy(columns) => NormalizedTableSpec {
                name: name.clone(),
                columns: normalize_path_map(columns),
                carry: IndexMap::new(),
                row_paths: Vec::new(),
                context_paths: Vec::new(),
                row_index_col: None,
                context_index_col: None,
                explicit_row_path: false,
            },
            XmlTableSpec::Structured(spec) => {
                let row_paths = normalize_path_list(spec.row_path.as_ref());
                NormalizedTableSpec {
                    name: name.clone(),
                    columns: normalize_path_map(&spec.columns),
                    carry: normalize_path_map(&spec.carry),
                    context_paths: normalize_path_list(spec.context_path.as_ref()),
                    explicit_row_path: !row_paths.is_empty(),
                    row_paths,
                    row_index_col: spec.row_index.clone(),
                    context_index_col: spec.context_index.clone(),
                }
            }
        })
        .collect()
}

fn resolve_table_specs(
    specs: Vec<NormalizedTableSpec>,
    real_paths: &HashSet<String>,
) -> Vec<ResolvedTableSpec> {
    specs
        .into_iter()
        .filter_map(|spec| {
            let columns = spec
                .columns
                .iter()
                .filter_map(|(path, column)| {
                    if real_paths.contains(path) {
                        Some((path.clone(), column.clone()))
                    } else {
                        None
                    }
                })
                .collect::<IndexMap<_, _>>();

            if columns.is_empty() {
                return None;
            }

            let carry = spec
                .carry
                .iter()
                .filter_map(|(path, column)| {
                    if real_paths.contains(path) {
                        Some((path.clone(), column.clone()))
                    } else {
                        None
                    }
                })
                .collect::<IndexMap<_, _>>();

            let row_boundary = if spec.explicit_row_path {
                select_first_existing_path(&spec.row_paths, real_paths)?
            } else {
                infer_row_boundary(columns.keys())
            };

            let context_path = select_context_path(&spec.context_paths, &row_boundary, real_paths);
            let mut output_columns = IndexSet::new();

            for column in columns.values() {
                output_columns.insert(column.clone());
            }
            for column in carry.values() {
                output_columns.insert(column.clone());
            }
            if let Some(column) = &spec.row_index_col {
                output_columns.insert(column.clone());
            }
            if let Some(column) = &spec.context_index_col {
                output_columns.insert(column.clone());
            }
            output_columns.insert("_table".to_string());

            Some(ResolvedTableSpec {
                name: spec.name,
                columns,
                carry,
                row_boundary,
                context_path,
                row_index_col: spec.row_index_col,
                context_index_col: spec.context_index_col,
                explicit_row_path: spec.explicit_row_path,
                output_columns: output_columns.into_iter().collect(),
            })
        })
        .collect()
}

fn parse_resolved_tables(bytes: &[u8], specs: Vec<ResolvedTableSpec>) -> Result<Vec<XmlTable>> {
    let path_index = build_path_index(&specs);
    let mut states = specs.into_iter().map(TableState::new).collect::<Vec<_>>();
    let mut reader = xml_reader(bytes);
    let mut buf = Vec::new();
    let mut stack: Vec<ElementFrame> = Vec::new();

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Start(e) => {
                if let Some(parent) = stack.last_mut() {
                    parent.has_child = true;
                }
                stack.push(ElementFrame {
                    tag: normalize_tag(e.name()),
                    attributes: collect_attributes(&reader, &e)?,
                    text: String::new(),
                    has_child: false,
                });
                let path = current_path(&stack);
                handle_context_start(&path, &path_index, &mut states);
            }
            Event::Empty(e) => {
                if let Some(parent) = stack.last_mut() {
                    parent.has_child = true;
                }
                stack.push(ElementFrame {
                    tag: normalize_tag(e.name()),
                    attributes: collect_attributes(&reader, &e)?,
                    text: String::new(),
                    has_child: false,
                });
                let path = current_path(&stack);
                handle_context_start(&path, &path_index, &mut states);
                handle_frame_text(&path, &path_index, &mut states, &stack);
                handle_attributes(&path, &path_index, &mut states, &stack);
                handle_row_boundary(&path, &path_index, &mut states);
                handle_context_end(&path, &path_index, &mut states);
                stack.pop();
            }
            Event::Text(e) => {
                if let Some(frame) = stack.last_mut() {
                    if !frame.has_child {
                        frame.text.push_str(&e.unescape()?);
                    }
                }
            }
            Event::CData(e) => {
                if let Some(frame) = stack.last_mut() {
                    if !frame.has_child {
                        frame.text.push_str(&String::from_utf8_lossy(e.as_ref()));
                    }
                }
            }
            Event::End(_) => {
                if !stack.is_empty() {
                    let path = current_path(&stack);
                    handle_frame_text(&path, &path_index, &mut states, &stack);
                    handle_attributes(&path, &path_index, &mut states, &stack);
                    handle_row_boundary(&path, &path_index, &mut states);
                    handle_context_end(&path, &path_index, &mut states);
                    stack.pop();
                }
            }
            Event::Eof => break,
            _ => {}
        }

        buf.clear();
    }

    let mut tables = Vec::new();
    for state in &mut states {
        state.finish();
    }
    for state in states {
        if !state.rows.is_empty() {
            tables.push(state.into_table());
        }
    }

    Ok(tables)
}

fn build_path_index(specs: &[ResolvedTableSpec]) -> PathIndex {
    let mut text_actions: HashMap<String, Vec<PathAction>> = HashMap::new();
    let mut attr_actions: HashMap<String, Vec<PathAction>> = HashMap::new();
    let mut row_boundaries: HashMap<String, Vec<usize>> = HashMap::new();
    let mut context_starts: HashMap<String, Vec<usize>> = HashMap::new();
    let mut context_ends: HashMap<String, Vec<usize>> = HashMap::new();

    for (table_id, spec) in specs.iter().enumerate() {
        for (path, column) in &spec.columns {
            insert_action(
                path,
                PathAction {
                    table_id,
                    target: ValueTarget::Column,
                    column: column.clone(),
                },
                &mut text_actions,
                &mut attr_actions,
            );
        }

        for (path, column) in &spec.carry {
            let target = if is_context_carry_path(spec, path) {
                ValueTarget::ContextCarry
            } else {
                ValueTarget::GlobalCarry
            };
            insert_action(
                path,
                PathAction {
                    table_id,
                    target,
                    column: column.clone(),
                },
                &mut text_actions,
                &mut attr_actions,
            );
        }

        row_boundaries
            .entry(spec.row_boundary.clone())
            .or_default()
            .push(table_id);

        if let Some(context_path) = &spec.context_path {
            context_starts
                .entry(context_path.clone())
                .or_default()
                .push(table_id);
            context_ends
                .entry(context_path.clone())
                .or_default()
                .push(table_id);
        }
    }

    PathIndex {
        text_actions,
        attr_actions,
        row_boundaries,
        context_starts,
        context_ends,
    }
}

fn insert_action(
    path: &str,
    action: PathAction,
    text_actions: &mut HashMap<String, Vec<PathAction>>,
    attr_actions: &mut HashMap<String, Vec<PathAction>>,
) {
    if path.contains("/@") {
        attr_actions
            .entry(path.to_string())
            .or_default()
            .push(action);
    } else {
        text_actions
            .entry(path.to_string())
            .or_default()
            .push(action);
    }
}

fn handle_context_start(path: &str, path_index: &PathIndex, states: &mut [TableState]) {
    if let Some(table_ids) = path_index.context_starts.get(path) {
        for table_id in table_ids {
            states[*table_id].start_context();
        }
    }
}

fn handle_context_end(path: &str, path_index: &PathIndex, states: &mut [TableState]) {
    if let Some(table_ids) = path_index.context_ends.get(path) {
        for table_id in table_ids {
            states[*table_id].finalize_context_carry();
        }
    }
}

fn handle_row_boundary(path: &str, path_index: &PathIndex, states: &mut [TableState]) {
    if let Some(table_ids) = path_index.row_boundaries.get(path) {
        for table_id in table_ids {
            states[*table_id].emit_row();
        }
    }
}

fn handle_text(path: &str, value: String, path_index: &PathIndex, states: &mut [TableState]) {
    if let Some(actions) = path_index.text_actions.get(path) {
        for action in actions {
            states[action.table_id].push_value(action.target, &action.column, value.clone());
        }
    }
}

fn handle_frame_text(
    path: &str,
    path_index: &PathIndex,
    states: &mut [TableState],
    stack: &[ElementFrame],
) {
    let Some(frame) = stack.last() else {
        return;
    };
    let value = frame.text.trim();
    if !value.is_empty() {
        handle_text(path, value.to_string(), path_index, states);
    }
}

fn handle_attributes(
    path: &str,
    path_index: &PathIndex,
    states: &mut [TableState],
    stack: &[ElementFrame],
) {
    let Some(frame) = stack.last() else {
        return;
    };

    for (attr_name, value) in &frame.attributes {
        let attr_path = format!("{path}/@{attr_name}");

        let Some(actions) = path_index.attr_actions.get(&attr_path) else {
            continue;
        };

        for action in actions {
            states[action.table_id].push_value(action.target, &action.column, value.clone());
        }
    }
}

fn collect_attributes<R: std::io::BufRead>(
    reader: &Reader<R>,
    start: &BytesStart<'_>,
) -> Result<Vec<(String, String)>> {
    let mut attributes = Vec::new();

    for attribute in start.attributes().with_checks(false) {
        let attribute = attribute?;
        attributes.push((
            normalize_attr_name(attribute.key),
            attribute
                .decode_and_unescape_value(reader.decoder())?
                .into_owned(),
        ));
    }

    Ok(attributes)
}

fn normalize_path_map(paths: &IndexMap<String, String>) -> IndexMap<String, String> {
    paths
        .iter()
        .map(|(path, column)| (normalize_path(path), column.clone()))
        .collect()
}

fn normalize_path_list(paths: Option<&XmlPathList>) -> Vec<String> {
    match paths {
        Some(XmlPathList::One(path)) => vec![normalize_path(path)],
        Some(XmlPathList::Many(paths)) => paths
            .iter()
            .filter_map(|path| {
                let normalized = normalize_path(path);
                if normalized.is_empty() {
                    None
                } else {
                    Some(normalized)
                }
            })
            .collect(),
        None => Vec::new(),
    }
}

fn normalize_path(path: &str) -> String {
    let path = path.trim();
    if path.is_empty() {
        return String::new();
    }
    if path.starts_with('/') {
        path.to_ascii_lowercase()
    } else {
        format!("/{path}").to_ascii_lowercase()
    }
}

fn select_first_existing_path(paths: &[String], real_paths: &HashSet<String>) -> Option<String> {
    paths
        .iter()
        .find(|path| !path.is_empty() && real_paths.contains(*path))
        .cloned()
}

fn select_context_path(
    paths: &[String],
    row_boundary: &str,
    real_paths: &HashSet<String>,
) -> Option<String> {
    if paths.is_empty() {
        return None;
    }

    for path in paths {
        if !path.is_empty()
            && real_paths.contains(path)
            && (row_boundary == path || row_boundary.starts_with(&format!("{path}/")))
        {
            return Some(path.clone());
        }
    }

    paths
        .iter()
        .find(|path| !path.is_empty() && real_paths.contains(*path))
        .cloned()
}

fn infer_row_boundary<'a>(paths: impl IntoIterator<Item = &'a String>) -> String {
    let split_paths = paths
        .into_iter()
        .map(|path| {
            path.split("/@")
                .next()
                .unwrap_or(path)
                .trim_matches('/')
                .split('/')
                .filter(|segment| !segment.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>()
        })
        .filter(|segments| !segments.is_empty())
        .collect::<Vec<_>>();

    let Some(first) = split_paths.first() else {
        return String::new();
    };

    let mut prefix = Vec::new();
    for idx in 0..first.len() {
        let segment = &first[idx];
        if split_paths
            .iter()
            .all(|segments| segments.get(idx) == Some(segment))
        {
            prefix.push(segment.clone());
        } else {
            break;
        }
    }

    if prefix.is_empty() {
        String::new()
    } else {
        format!("/{}", prefix.join("/"))
    }
}

fn is_context_carry_path(spec: &ResolvedTableSpec, path: &str) -> bool {
    spec.context_path.as_ref().is_some_and(|context_path| {
        path == context_path || path.starts_with(&format!("{context_path}/"))
    })
}

fn accumulator_for<'a>(
    columns: impl IntoIterator<Item = &'a String>,
) -> IndexMap<String, Vec<String>> {
    let mut accumulator = IndexMap::new();
    for column in columns {
        accumulator.entry(column.clone()).or_insert_with(Vec::new);
    }
    accumulator
}

fn clear_accumulator(accumulator: &mut IndexMap<String, Vec<String>>) {
    for values in accumulator.values_mut() {
        values.clear();
    }
}

fn push_accumulator_value(
    accumulator: &mut IndexMap<String, Vec<String>>,
    column: &str,
    value: String,
) {
    accumulator
        .entry(column.to_string())
        .or_insert_with(Vec::new)
        .push(value);
}

fn apply_accumulator_to_row(
    row: &mut XmlRow,
    accumulator: &IndexMap<String, Vec<String>>,
    only_if_empty: bool,
) {
    for (column, values) in accumulator {
        if values.is_empty() {
            continue;
        }

        if only_if_empty && row.get(column).is_some_and(Option::is_some) {
            continue;
        }

        row.insert(column.clone(), Some(join_values(values)));
    }
}

fn join_values(values: &[String]) -> String {
    values.join("|")
}

fn row_has_non_meta_value(row: &XmlRow, spec: &ResolvedTableSpec) -> bool {
    row.iter().any(|(column, value)| {
        if column == "_table"
            || spec.row_index_col.as_ref() == Some(column)
            || spec.context_index_col.as_ref() == Some(column)
        {
            return false;
        }

        value.as_ref().is_some_and(|value| !value.trim().is_empty())
    })
}

fn current_path<T: PathStackItem>(stack: &[T]) -> String {
    if stack.is_empty() {
        String::new()
    } else {
        format!(
            "/{}",
            stack
                .iter()
                .map(PathStackItem::path_segment)
                .collect::<Vec<_>>()
                .join("/")
        )
    }
}

trait PathStackItem {
    fn path_segment(&self) -> &str;
}

impl PathStackItem for String {
    fn path_segment(&self) -> &str {
        self
    }
}

impl PathStackItem for ElementFrame {
    fn path_segment(&self) -> &str {
        &self.tag
    }
}

fn normalize_tag(name: QName<'_>) -> String {
    let local_name = name.local_name();
    String::from_utf8_lossy(local_name.as_ref()).to_ascii_lowercase()
}

fn normalize_attr_name(name: QName<'_>) -> String {
    String::from_utf8_lossy(name.as_ref()).to_ascii_lowercase()
}

fn xml_reader(bytes: &[u8]) -> Reader<Cursor<&[u8]>> {
    let mut reader = Reader::from_reader(Cursor::new(bytes));
    reader.config_mut().trim_text(true);
    reader
}
