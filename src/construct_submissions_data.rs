use std::fs::File;
use std::io::{Cursor, Read, Seek};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use arrow_array::{ArrayRef, RecordBatch, StringArray, UInt64Array};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;
use serde_json::Value;
use tracing::{info, warn};
use zip::ZipArchive;

use crate::common::sec_user_agent;

const SUBMISSIONS_ZIP_URL: &str =
    "https://www.sec.gov/Archives/edgar/daily-index/bulkdata/submissions.zip";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConstructSubmissionsDataStats {
    pub files_processed: usize,
    pub files_skipped: usize,
    pub filings_written: usize,
}

struct SubmissionRecords {
    ciks: Vec<u64>,
    accession_numbers: Vec<String>,
    filing_dates: Vec<String>,
    forms: Vec<String>,
}

pub async fn construct_submissions_data(
    output_path: impl Into<PathBuf>,
    submissions_zip_path: Option<PathBuf>,
) -> Result<ConstructSubmissionsDataStats> {
    let output_path = output_path.into();

    if let Some(submissions_zip_path) = submissions_zip_path {
        return tokio::task::spawn_blocking(move || {
            construct_submissions_data_from_zip(output_path, submissions_zip_path)
        })
        .await?;
    }

    let zip_bytes = download_submissions_zip().await?;
    tokio::task::spawn_blocking(move || {
        let cursor = Cursor::new(zip_bytes);
        write_submissions_parquet(cursor, output_path)
    })
    .await?
}

pub fn construct_submissions_data_from_zip(
    output_path: impl AsRef<Path>,
    submissions_zip_path: impl AsRef<Path>,
) -> Result<ConstructSubmissionsDataStats> {
    let file = File::open(submissions_zip_path.as_ref()).with_context(|| {
        format!(
            "failed to open submissions zip {}",
            submissions_zip_path.as_ref().display()
        )
    })?;

    write_submissions_parquet(file, output_path)
}

async fn download_submissions_zip() -> Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .user_agent(sec_user_agent())
        .build()
        .context("failed to build SEC HTTP client")?;

    info!(url = SUBMISSIONS_ZIP_URL, "Downloading SEC submissions zip");
    let response = client
        .get(SUBMISSIONS_ZIP_URL)
        .send()
        .await
        .context("failed to download SEC submissions zip")?
        .error_for_status()
        .context("SEC submissions zip request failed")?;

    let bytes = response
        .bytes()
        .await
        .context("failed to read SEC submissions zip response")?
        .to_vec();
    info!(bytes = bytes.len(), "Downloaded SEC submissions zip");
    Ok(bytes)
}

fn write_submissions_parquet<R>(
    submissions_zip: R,
    output_path: impl AsRef<Path>,
) -> Result<ConstructSubmissionsDataStats>
where
    R: Read + Seek,
{
    let mut archive =
        ZipArchive::new(submissions_zip).context("failed to open submissions zip archive")?;
    let output = File::create(output_path.as_ref()).with_context(|| {
        format!(
            "failed to create submissions parquet {}",
            output_path.as_ref().display()
        )
    })?;
    let schema = submissions_schema();
    let props = WriterProperties::builder()
        .set_compression(Compression::SNAPPY)
        .build();
    let mut writer = ArrowWriter::try_new(output, schema.clone(), Some(props))
        .context("failed to create parquet writer")?;

    let mut stats = ConstructSubmissionsDataStats {
        files_processed: 0,
        files_skipped: 0,
        filings_written: 0,
    };

    info!(
        zip_entries = archive.len(),
        "Processing SEC submissions zip"
    );

    for index in 0..archive.len() {
        let mut file = match archive.by_index(index) {
            Ok(file) => file,
            Err(error) => {
                stats.files_skipped += 1;
                warn!(%index, %error, "Skipping unreadable submissions zip entry");
                continue;
            }
        };

        let filename = file.name().to_string();
        if !filename.starts_with("CIK") {
            continue;
        }

        match process_submission_file(&filename, &mut file) {
            Ok(records) => {
                let row_count = records.len();
                if row_count > 0 {
                    let batch = records.into_record_batch(schema.clone())?;
                    writer
                        .write(&batch)
                        .context("failed to write submissions parquet batch")?;
                }
                stats.filings_written += row_count;
                stats.files_processed += 1;

                if stats.files_processed % 100 == 0 {
                    info!(
                        files_processed = stats.files_processed,
                        files_skipped = stats.files_skipped,
                        filings_written = stats.filings_written,
                        "Processed submissions files"
                    );
                }
            }
            Err(error) => {
                stats.files_skipped += 1;
                warn!(filename, %error, "Skipping submissions file");
            }
        }
    }

    writer
        .close()
        .context("failed to close submissions parquet writer")?;
    info!(
        files_processed = stats.files_processed,
        files_skipped = stats.files_skipped,
        filings_written = stats.filings_written,
        "Finished constructing submissions data"
    );
    Ok(stats)
}

fn process_submission_file<R>(filename: &str, reader: R) -> Result<SubmissionRecords>
where
    R: Read,
{
    let cik = cik_from_filename(filename)?;
    let data: Value = serde_json::from_reader(reader)
        .with_context(|| format!("failed to parse submissions JSON {filename}"))?;
    let filings = filings_data(filename, &data)?;

    let accessions = field_array(filings, "accessionNumber")?;
    let filing_dates = field_array(filings, "filingDate")?;
    let forms = field_array(filings, "form")?;

    if filing_dates.len() < accessions.len() || forms.len() < accessions.len() {
        return Err(anyhow!("submissions fields have mismatched lengths"));
    }

    let mut records = SubmissionRecords::with_capacity(accessions.len());
    for index in 0..accessions.len() {
        records.ciks.push(cik);
        records
            .accession_numbers
            .push(parquet_value(&accessions[index]));
        records
            .filing_dates
            .push(parquet_value(&filing_dates[index]));
        records.forms.push(parquet_value(&forms[index]));
    }

    Ok(records)
}

fn cik_from_filename(filename: &str) -> Result<u64> {
    filename
        .split('.')
        .next()
        .and_then(|stem| stem.split('-').next())
        .and_then(|stem| stem.strip_prefix("CIK"))
        .ok_or_else(|| anyhow!("filename does not start with CIK: {filename}"))?
        .parse::<u64>()
        .with_context(|| format!("failed to parse CIK from {filename}"))
}

fn filings_data<'a>(filename: &str, data: &'a Value) -> Result<&'a Value> {
    if filename.contains("submissions") {
        return Ok(data);
    }

    data.get("filings")
        .and_then(|filings| filings.get("recent"))
        .ok_or_else(|| anyhow!("missing filings.recent"))
}

fn field_array<'a>(filings: &'a Value, field: &str) -> Result<&'a [Value]> {
    filings
        .get(field)
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .ok_or_else(|| anyhow!("missing array field {field}"))
}

fn parquet_value(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(value) => value.clone(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::Array(_) | Value::Object(_) => value.to_string(),
    }
}

fn submissions_schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("cik", DataType::UInt64, false),
        Field::new("accessionNumber", DataType::Utf8, false),
        Field::new("filingDate", DataType::Utf8, false),
        Field::new("form", DataType::Utf8, false),
    ]))
}

impl SubmissionRecords {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            ciks: Vec::with_capacity(capacity),
            accession_numbers: Vec::with_capacity(capacity),
            filing_dates: Vec::with_capacity(capacity),
            forms: Vec::with_capacity(capacity),
        }
    }

    fn len(&self) -> usize {
        self.accession_numbers.len()
    }

    fn into_record_batch(self, schema: SchemaRef) -> Result<RecordBatch> {
        let columns: Vec<ArrayRef> = vec![
            Arc::new(UInt64Array::from(self.ciks)),
            Arc::new(StringArray::from(self.accession_numbers)),
            Arc::new(StringArray::from(self.filing_dates)),
            Arc::new(StringArray::from(self.forms)),
        ];

        RecordBatch::try_new(schema, columns).context("failed to build submissions record batch")
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
    use zip::write::SimpleFileOptions;

    use super::*;

    #[test]
    fn writes_sec_field_names_without_mapping() {
        let zip_bytes = sample_zip(
            "CIK0000320193.json",
            br#"{
                "filings": {
                    "recent": {
                        "accessionNumber": ["0000320193-24-000123"],
                        "filingDate": ["2024-10-31"],
                        "form": ["10-Q"]
                    }
                }
            }"#,
        );

        let output = tempfile_path("submissions-field-names.parquet");
        let stats = write_submissions_parquet(Cursor::new(zip_bytes), &output).unwrap();
        let batch = read_first_batch(&output);

        assert_eq!(stats.files_processed, 1);
        assert_eq!(stats.filings_written, 1);
        assert_eq!(
            batch
                .schema()
                .fields()
                .iter()
                .map(|field| field.name())
                .collect::<Vec<_>>(),
            vec!["cik", "accessionNumber", "filingDate", "form"]
        );
        assert_eq!(
            batch
                .column(0)
                .as_any()
                .downcast_ref::<UInt64Array>()
                .unwrap()
                .value(0),
            320193
        );
        assert_eq!(
            batch
                .column(1)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .value(0),
            "0000320193-24-000123"
        );
        assert_eq!(
            batch
                .column(3)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .value(0),
            "10-Q"
        );

        let _ = std::fs::remove_file(output);
    }

    #[test]
    fn reads_submission_shard_shape() {
        let zip_bytes = sample_zip(
            "CIK0000000001-submissions-001.json",
            br#"{
                "accessionNumber": ["0000000001-24-000001"],
                "filingDate": ["2024-01-02"],
                "form": ["8-K"]
            }"#,
        );

        let output = tempfile_path("submissions-shard.parquet");
        let stats = write_submissions_parquet(Cursor::new(zip_bytes), &output).unwrap();
        let batch = read_first_batch(&output);

        assert_eq!(stats.files_processed, 1);
        assert_eq!(stats.filings_written, 1);
        assert_eq!(
            batch
                .column(0)
                .as_any()
                .downcast_ref::<UInt64Array>()
                .unwrap()
                .value(0),
            1
        );
        assert_eq!(
            batch
                .column(3)
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap()
                .value(0),
            "8-K"
        );

        let _ = std::fs::remove_file(output);
    }

    fn sample_zip(filename: &str, json: &[u8]) -> Vec<u8> {
        let mut zip_bytes = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut zip_bytes);
            zip.start_file(filename, SimpleFileOptions::default())
                .unwrap();
            zip.write_all(json).unwrap();
            zip.finish().unwrap();
        }
        zip_bytes.into_inner()
    }

    fn tempfile_path(filename: &str) -> PathBuf {
        std::env::temp_dir().join(format!("secinfra-{}-{}", std::process::id(), filename))
    }

    fn read_first_batch(path: &Path) -> RecordBatch {
        let file = File::open(path).unwrap();
        let mut reader = ParquetRecordBatchReaderBuilder::try_new(file)
            .unwrap()
            .build()
            .unwrap();
        reader.next().unwrap().unwrap()
    }
}
