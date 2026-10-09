use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::thread;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, ensure};
use arrow_array::builder::{Int64Builder, ListBuilder, StringBuilder};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field, Schema};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use serde_json::Value;
use tracing::info;
use zip::ZipArchive;

use crate::common::sec_user_agent;

const SUBMISSIONS_ZIP_URL: &str =
    "https://www.sec.gov/Archives/edgar/daily-index/bulkdata/submissions.zip";
const BATCH_FILES: usize = 1_000;
const STRING_FIELDS: [&str; 13] = [
    "name",
    "entityType",
    "sic",
    "sicDescription",
    "ownerOrg",
    "ein",
    "description",
    "category",
    "fiscalYearEnd",
    "stateOfIncorporation",
    "stateOfIncorporationDescription",
    "phone",
    "flags",
];
const ADDRESS_FIELDS: [&str; 6] = [
    "street1",
    "street2",
    "city",
    "stateOrCountry",
    "zipCode",
    "stateOrCountryDescription",
];
const INDICATOR_FIELDS: [&str; 2] = [
    "insiderTransactionForOwnerExists",
    "insiderTransactionForIssuerExists",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConstructFilerMetadataStats {
    pub files_processed: usize,
    pub files_skipped: usize,
    pub filers_written: usize,
    pub names_written: usize,
    pub batches_written: usize,
}

struct FilerRow {
    cik: i64,
    strings: Vec<Option<String>>,
    start_date: Option<String>,
    tickers: Option<Vec<Option<String>>>,
    exchanges: Option<Vec<Option<String>>>,
    indicators: [Option<i64>; 2],
    names: Vec<NameRow>,
}

struct NameRow {
    name: Option<String>,
    start_date: Option<String>,
    end_date: Option<String>,
}

struct TempZip(PathBuf);

impl Drop for TempZip {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Construct filer_metadata.parquet and filer_names.parquet in output_dir.
/// Downloads SEC's bulk submissions ZIP when no local path is supplied.
/// Sets the SEC user agent using SEC_USER_AGENT, like other secinfra downloads.
pub async fn construct_filer_metadata(
    output_dir: impl Into<PathBuf>,
    submissions_zip_path: Option<PathBuf>,
    threads: Option<usize>,
) -> Result<ConstructFilerMetadataStats> {
    let output_dir = output_dir.into();
    let threads = resolve_threads(threads)?;
    if let Some(path) = submissions_zip_path {
        return tokio::task::spawn_blocking(move || {
            construct_filer_metadata_from_zip(output_dir, path, Some(threads))
        })
        .await?;
    }

    let started_at = Instant::now();
    let path = std::env::temp_dir().join(format!(
        "secinfra-filers-{}-{}.zip",
        std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos(),
    ));
    let temp_zip = TempZip(path.clone());
    let client = reqwest::Client::builder()
        .user_agent(sec_user_agent())
        .build()?;
    let mut response = client
        .get(SUBMISSIONS_ZIP_URL)
        .send()
        .await?
        .error_for_status()
        .context("SEC submissions ZIP request failed")?;
    // Write chunks directly to disk instead of retaining the bulk ZIP in memory.
    let mut file = File::create(&path).context("failed to create temporary submissions ZIP")?;
    let mut bytes = 0;
    while let Some(chunk) = response.chunk().await? {
        file.write_all(&chunk)?;
        bytes += chunk.len();
    }
    file.flush()?;
    drop(file);
    info!(
        bytes,
        elapsed_ms = started_at.elapsed().as_millis(),
        "Downloaded filer submissions ZIP"
    );
    tokio::task::spawn_blocking(move || {
        construct_filer_metadata_from_zip(output_dir, &temp_zip.0, Some(threads))
    })
    .await?
}

/// Read primary CIK JSON entries from a local ZIP and write two Zstd Parquet tables.
/// Historical CIK##########-submissions-###.json chunks contain filings only and
/// are excluded. Malformed primary records fail the run rather than silently
/// publishing an incomplete snapshot. Dates preserve the Python extractor's rules.
pub fn construct_filer_metadata_from_zip(
    output_dir: impl AsRef<Path>,
    submissions_zip_path: impl AsRef<Path>,
    threads: Option<usize>,
) -> Result<ConstructFilerMetadataStats> {
    let started_at = Instant::now();
    let threads = resolve_threads(threads)?;
    let zip_path = submissions_zip_path.as_ref();
    let mut archive = ZipArchive::new(File::open(zip_path)?)?;
    let mut filenames = Vec::new();
    for index in 0..archive.len() {
        let file = archive.by_index(index)?;
        if !file.is_dir() && is_primary_filer(file.name()) {
            filenames.push(file.name().to_owned());
        }
    }
    ensure!(
        !filenames.is_empty(),
        "submissions ZIP contains no primary filer JSON files"
    );
    let skipped = archive.len() - filenames.len();
    drop(archive);

    let output_dir = output_dir.as_ref();
    fs::create_dir_all(output_dir)?;
    let metadata_schema = std::sync::Arc::new(metadata_schema());
    let names_schema = std::sync::Arc::new(names_schema());
    let properties = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(1)?))
        .build();
    let mut metadata_writer = ArrowWriter::try_new(
        File::create(output_dir.join("filer_metadata.parquet"))?,
        metadata_schema.clone(),
        Some(properties.clone()),
    )?;
    let mut names_writer = ArrowWriter::try_new(
        File::create(output_dir.join("filer_names.parquet"))?,
        names_schema.clone(),
        Some(properties),
    )?;
    let mut stats = ConstructFilerMetadataStats {
        files_processed: 0,
        files_skipped: skipped,
        filers_written: 0,
        names_written: 0,
        batches_written: 0,
    };
    let worker_count = threads.min(filenames.len());
    let cancel = AtomicBool::new(false);
    let (tx, rx) = mpsc::sync_channel::<Result<Vec<FilerRow>>>(worker_count);
    info!(
        files = filenames.len(),
        threads = worker_count,
        "Constructing filer metadata"
    );

    thread::scope(|scope| -> Result<()> {
        for worker in 0..worker_count {
            let tx = tx.clone();
            let filenames = &filenames;
            let cancel = &cancel;
            scope.spawn(move || {
                let result = (|| -> Result<()> {
                    let mut archive = ZipArchive::new(File::open(zip_path)?)?;
                    for batch in filenames
                        .chunks(BATCH_FILES)
                        .skip(worker)
                        .step_by(worker_count)
                    {
                        if cancel.load(Ordering::Relaxed) {
                            break;
                        }
                        let mut rows = Vec::with_capacity(batch.len());
                        for filename in batch {
                            if cancel.load(Ordering::Relaxed) {
                                return Ok(());
                            }
                            let mut file = archive.by_name(filename)?;
                            let mut bytes = Vec::new();
                            file.read_to_end(&mut bytes)?;
                            let data: Value = serde_json::from_slice(&bytes)
                                .with_context(|| format!("invalid filer JSON in {filename}"))?;
                            rows.push(extract_filer(&data).with_context(|| {
                                format!("invalid filer metadata in {filename}")
                            })?);
                        }
                        if tx.send(Ok(rows)).is_err() {
                            break;
                        }
                    }
                    Ok(())
                })();
                if let Err(error) = result {
                    cancel.store(true, Ordering::Relaxed);
                    let _ = tx.send(Err(error));
                }
            });
        }
        drop(tx);
        let mut first_error = None;
        for result in rx {
            if first_error.is_some() {
                continue;
            }
            let result = result.and_then(|rows| {
                let names_count = rows.iter().map(|row| row.names.len()).sum::<usize>();
                metadata_writer.write(&metadata_batch(&rows, metadata_schema.clone())?)?;
                names_writer.write(&names_batch(&rows, names_schema.clone())?)?;
                stats.files_processed += rows.len();
                stats.filers_written += rows.len();
                stats.names_written += names_count;
                stats.batches_written += 1;
                Ok(())
            });
            if let Err(error) = result {
                cancel.store(true, Ordering::Relaxed);
                first_error = Some(error);
            }
        }
        if let Some(error) = first_error {
            return Err(error);
        }
        Ok(())
    })?;
    metadata_writer.close()?;
    names_writer.close()?;
    ensure!(
        stats.filers_written == filenames.len(),
        "incomplete filer snapshot"
    );
    info!(
        filers = stats.filers_written,
        names = stats.names_written,
        elapsed_ms = started_at.elapsed().as_millis(),
        "Finished constructing filer metadata"
    );
    Ok(stats)
}

fn resolve_threads(threads: Option<usize>) -> Result<usize> {
    let threads = threads.unwrap_or(1);
    ensure!(threads > 0, "filer metadata threads must be at least 1");
    Ok(threads)
}

fn is_primary_filer(filename: &str) -> bool {
    filename
        .strip_prefix("CIK")
        .and_then(|s| s.strip_suffix(".json"))
        .is_some_and(|cik| cik.len() == 10 && cik.bytes().all(|b| b.is_ascii_digit()))
}

fn string_value(value: &Value) -> Option<String> {
    match value {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => Some(other.to_string()),
    }
}

fn date_value(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| s.split('T').next().unwrap().to_owned())
}

fn integer_value(value: &Value) -> Result<Option<i64>> {
    match value {
        Value::Null => Ok(None),
        Value::Bool(value) => Ok(Some(i64::from(*value))),
        Value::String(text) if text.is_empty() => Ok(None),
        Value::String(text) => Ok(Some(text.parse()?)),
        other => other
            .as_i64()
            .map(Some)
            .ok_or_else(|| anyhow!("expected integer, got {other}")),
    }
}

fn string_list(value: &Value) -> Result<Option<Vec<Option<String>>>> {
    if value.is_null() {
        return Ok(None);
    }
    let values = value
        .as_array()
        .ok_or_else(|| anyhow!("expected string array"))?;
    values
        .iter()
        .map(|value| match value {
            Value::Null => Ok(None),
            Value::String(text) => Ok(Some(text.clone())),
            _ => Err(anyhow!("expected string or null array item")),
        })
        .collect::<Result<Vec<_>>>()
        .map(Some)
}

fn extract_filer(data: &Value) -> Result<FilerRow> {
    let cik = integer_value(&data["cik"])?.ok_or_else(|| anyhow!("missing CIK"))?;
    ensure!(cik > 0, "CIK must be positive");
    let mut strings = STRING_FIELDS
        .iter()
        .map(|field| string_value(&data[*field]))
        .collect::<Vec<_>>();
    for address in ["mailing", "business"] {
        for field in ADDRESS_FIELDS {
            strings.push(string_value(&data["addresses"][address][field]));
        }
    }
    let mut names = Vec::new();
    let mut earliest = None;
    let mut latest_end = None;
    if let Some(former_names) = data["formerNames"].as_array() {
        for former in former_names.iter().filter(|v| v.is_object()) {
            let start_date = date_value(&former["from"]);
            let end_date = date_value(&former["to"]);
            if let Some(date) = &start_date {
                if earliest.as_ref().is_none_or(|previous| date < previous) {
                    earliest = Some(date.clone());
                }
            }
            if let Some(date) = &end_date {
                if latest_end.as_ref().is_none_or(|previous| date > previous) {
                    latest_end = Some(date.clone());
                }
            }
            names.push(NameRow {
                name: string_value(&former["name"]),
                start_date,
                end_date,
            });
        }
    }
    // Match Python: prefer former-name starts, then historical file ranges,
    // then recent filing dates. These dates describe SEC history, not incorporation.
    if earliest.is_none() {
        earliest = data["filings"]["files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|file| date_value(&file["filingFrom"]))
            .min();
    }
    if earliest.is_none() {
        earliest = data["filings"]["recent"]["filingDate"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(date_value)
            .min();
    }
    names.push(NameRow {
        name: strings[0].clone(),
        start_date: latest_end.or_else(|| earliest.clone()),
        end_date: None,
    });
    Ok(FilerRow {
        cik,
        strings,
        start_date: earliest,
        tickers: string_list(&data["tickers"])?,
        exchanges: string_list(&data["exchanges"])?,
        indicators: [
            integer_value(&data[INDICATOR_FIELDS[0]])?,
            integer_value(&data[INDICATOR_FIELDS[1]])?,
        ],
        names,
    })
}

fn metadata_schema() -> Schema {
    let mut fields = vec![Field::new("cik", DataType::Int64, false)];
    for name in STRING_FIELDS {
        fields.push(Field::new(name, DataType::Utf8, true));
    }
    for address in ["mailing", "business"] {
        for field in ADDRESS_FIELDS {
            fields.push(Field::new(
                format!("{address}_{field}"),
                DataType::Utf8,
                true,
            ));
        }
    }
    fields.push(Field::new("start_date", DataType::Utf8, true));
    for name in ["tickers", "exchanges"] {
        fields.push(Field::new(
            name,
            DataType::List(std::sync::Arc::new(Field::new(
                "item",
                DataType::Utf8,
                true,
            ))),
            true,
        ));
    }
    for name in INDICATOR_FIELDS {
        fields.push(Field::new(name, DataType::Int64, true));
    }
    Schema::new(fields)
}

fn names_schema() -> Schema {
    Schema::new(vec![
        Field::new("cik", DataType::Int64, false),
        Field::new("name", DataType::Utf8, true),
        Field::new("start_date", DataType::Utf8, true),
        Field::new("end_date", DataType::Utf8, true),
    ])
}

fn metadata_batch(rows: &[FilerRow], schema: std::sync::Arc<Schema>) -> Result<RecordBatch> {
    let mut cik = Int64Builder::new();
    let mut strings = (0..STRING_FIELDS.len() + ADDRESS_FIELDS.len() * 2)
        .map(|_| StringBuilder::new())
        .collect::<Vec<_>>();
    let mut starts = StringBuilder::new();
    let mut tickers = ListBuilder::new(StringBuilder::new());
    let mut exchanges = ListBuilder::new(StringBuilder::new());
    let mut indicators = [Int64Builder::new(), Int64Builder::new()];
    for row in rows {
        cik.append_value(row.cik);
        for (builder, value) in strings.iter_mut().zip(&row.strings) {
            builder.append_option(value.as_deref());
        }
        starts.append_option(row.start_date.as_deref());
        for (builder, values) in [
            (&mut tickers, &row.tickers),
            (&mut exchanges, &row.exchanges),
        ] {
            if let Some(values) = values {
                for value in values {
                    builder.values().append_option(value.as_deref());
                }
                builder.append(true);
            } else {
                builder.append(false);
            }
        }
        for (builder, value) in indicators.iter_mut().zip(row.indicators) {
            builder.append_option(value);
        }
    }
    let mut arrays: Vec<ArrayRef> = vec![std::sync::Arc::new(cik.finish())];
    arrays.extend(
        strings
            .iter_mut()
            .map(|builder| std::sync::Arc::new(builder.finish()) as ArrayRef),
    );
    arrays.push(std::sync::Arc::new(starts.finish()));
    arrays.push(std::sync::Arc::new(tickers.finish()));
    arrays.push(std::sync::Arc::new(exchanges.finish()));
    arrays.extend(
        indicators
            .iter_mut()
            .map(|builder| std::sync::Arc::new(builder.finish()) as ArrayRef),
    );
    Ok(RecordBatch::try_new(schema, arrays)?)
}

fn names_batch(rows: &[FilerRow], schema: std::sync::Arc<Schema>) -> Result<RecordBatch> {
    let mut cik = Int64Builder::new();
    let mut name = StringBuilder::new();
    let mut starts = StringBuilder::new();
    let mut ends = StringBuilder::new();
    for row in rows {
        for record in &row.names {
            cik.append_value(row.cik);
            name.append_option(record.name.as_deref());
            starts.append_option(record.start_date.as_deref());
            ends.append_option(record.end_date.as_deref());
        }
    }
    Ok(RecordBatch::try_new(
        schema,
        vec![
            std::sync::Arc::new(cik.finish()),
            std::sync::Arc::new(name.finish()),
            std::sync::Arc::new(starts.finish()),
            std::sync::Arc::new(ends.finish()),
        ],
    )?)
}
