# Filer metadata

`construct_filer_metadata(output_dir, submissions_zip_path, threads)` writes two
Zstd-compressed Parquet files: `filer_metadata.parquet` and `filer_names.parquet`.
Use `construct_filer_metadata_from_zip` for synchronous local ZIP processing.
The async entry point downloads SEC's submissions ZIP if the local path is None;
set `SEC_USER_AGENT` to your name and contact email before downloading.

```bash
cargo run --release --example filer_metadata -- /tmp/filers /tmp/submissions.zip 4
# Download the SEC archive automatically:
cargo run --release --example filer_metadata -- /tmp/filers - 4
```

Only primary `CIK##########.json` files are processed; historical filing chunks
are excluded. Both tables include all filers, with no listed/unlisted split.
CIKs are signed 64-bit integers, tickers and exchanges are arrays of strings,
and the two insider-transaction indicators are nullable integers (0 or 1).
Other metadata fields are strings, including SIC and EIN to retain leading zeros.
Missing values are Parquet nulls, including null items in SEC exchange arrays. Mailing and business addresses are flattened.

The name table has `cik`, `name`, `start_date`, and `end_date`. Each filer has a
current name with a null end_date and zero or more former names. Dates are strings
in YYYY-MM-DD form. Date derivation matches the Python extractor: metadata
start_date uses the earliest former-name start, falling back to the earliest
historical filing range and then the earliest recent filing. The current name
starts at the latest former-name end, falling back to metadata start_date.
These are dates in SEC history, not incorporation dates.

The implementation uses bounded batches and configurable worker threads. Invalid
primary filer JSON fails the operation; callers must publish files only after a
successful return. The statistics report processed/skipped ZIP entries, filer and
name rows, and batches. Record order can vary with thread count.
