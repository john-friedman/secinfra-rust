use std::fs::File;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use arrow_array::{Array, Int64Array, ListArray, StringArray};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use secinfra::construct_filer_metadata_from_zip;
use serde_json::json;
use zip::{ZipWriter, write::SimpleFileOptions};

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "secinfra-filer-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn writes_all_filers_and_preserves_native_lists_and_name_dates() {
    let scratch = Scratch::new();
    let zip_path = scratch.0.join("submissions.zip");
    let mut zip = ZipWriter::new(File::create(&zip_path).unwrap());
    // SEC sometimes supplies CIK as a padded string and a null exchange item.
    let first = json!({
        "cik": "0000000001", "name": "Current Î©", "tickers": ["TEST"], "exchanges": [null],
        "description": "x".repeat(3000), "sic": "0123",
        "formerNames": [{"name": "Old", "from": "2002-01-01T00:00:00Z", "to": "2024-01-01T00:00:00Z"}],
        "filings": {"files": [{"filingFrom": "1995-01-01"}], "recent": {"filingDate": ["1990-01-01"]}},
    });
    zip.start_file("CIK0000000001.json", SimpleFileOptions::default())
        .unwrap();
    zip.write_all(first.to_string().as_bytes()).unwrap();
    for cik in 2..=2001 {
        zip.start_file(format!("CIK{cik:010}.json"), SimpleFileOptions::default())
            .unwrap();
        zip.write_all(json!({"cik": cik, "name": "No tickers", "tickers": [], "exchanges": [],
            "filings": {"files": [{"filingFrom": "2000-01-01"}], "recent": {"filingDate": ["1990-01-01"]}}
        }).to_string().as_bytes()).unwrap();
    }
    zip.start_file(
        "CIK0000000001-submissions-001.json",
        SimpleFileOptions::default(),
    )
    .unwrap();
    zip.write_all(b"{\"filingDate\":[\"1990-01-01\"]}").unwrap();
    zip.start_file("placeholder.txt", SimpleFileOptions::default())
        .unwrap();
    zip.finish().unwrap();

    for threads in [1, 4] {
        let output = scratch.0.join(format!("output-{threads}"));
        let stats = construct_filer_metadata_from_zip(&output, &zip_path, Some(threads)).unwrap();
        assert_eq!(stats.filers_written, 2001);
        assert_eq!(stats.names_written, 2002);
        assert_eq!(stats.files_skipped, 2);
        assert_eq!(stats.batches_written, 3);
        let reader = ParquetRecordBatchReaderBuilder::try_new(
            File::open(output.join("filer_metadata.parquet")).unwrap(),
        )
        .unwrap()
        .build()
        .unwrap();
        let mut saw_first = false;
        for batch in reader {
            let batch = batch.unwrap();
            assert!(batch.column_by_name("is_listed").is_none());
            let ciks = batch
                .column_by_name("cik")
                .unwrap()
                .as_any()
                .downcast_ref::<Int64Array>()
                .unwrap();
            let starts = batch
                .column_by_name("start_date")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            for row in 0..batch.num_rows() {
                if ciks.value(row) == 1 {
                    saw_first = true;
                    assert_eq!(starts.value(row), "2002-01-01");
                    let exchanges = batch
                        .column_by_name("exchanges")
                        .unwrap()
                        .as_any()
                        .downcast_ref::<ListArray>()
                        .unwrap();
                    let values = exchanges.value(row);
                    assert_eq!(values.len(), 1);
                    assert!(values.is_null(0));
                } else {
                    // Preserve Python's preference for historical file ranges.
                    assert_eq!(starts.value(row), "2000-01-01");
                }
            }
        }
        assert!(saw_first);
        let reader = ParquetRecordBatchReaderBuilder::try_new(
            File::open(output.join("filer_names.parquet")).unwrap(),
        )
        .unwrap()
        .build()
        .unwrap();
        let mut saw_current = false;
        for batch in reader {
            let batch = batch.unwrap();
            let names = batch
                .column_by_name("name")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let starts = batch
                .column_by_name("start_date")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            let ends = batch
                .column_by_name("end_date")
                .unwrap()
                .as_any()
                .downcast_ref::<StringArray>()
                .unwrap();
            for row in 0..batch.num_rows() {
                if names.value(row) == "Current Î©" {
                    saw_current = true;
                    assert_eq!(starts.value(row), "2024-01-01");
                    assert!(ends.is_null(row));
                }
            }
        }
        assert!(saw_current);
    }
}

#[test]
fn malformed_primary_records_and_empty_archives_fail() {
    let scratch = Scratch::new();
    for (label, filename, content) in [
        ("bad-json", "CIK0000000001.json", "{invalid"),
        ("missing-cik", "CIK0000000001.json", "{\"name\":\"Bad\"}"),
        ("no-filers", "placeholder.txt", ""),
    ] {
        let path = scratch.0.join(format!("{label}.zip"));
        let mut zip = ZipWriter::new(File::create(&path).unwrap());
        zip.start_file(filename, SimpleFileOptions::default())
            .unwrap();
        zip.write_all(content.as_bytes()).unwrap();
        zip.finish().unwrap();
        assert!(construct_filer_metadata_from_zip(scratch.0.join(label), path, Some(4)).is_err());
    }
    assert!(
        construct_filer_metadata_from_zip(&scratch.0, scratch.0.join("bad-json.zip"), Some(0))
            .is_err()
    );
}
