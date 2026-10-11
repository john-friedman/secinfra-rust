//! Read {"accession": integer, "metadata": {...}} lines from stdin and emit tables.
use secinfra::flatten_metadata;
use serde::Deserialize;
use serde_json::Value;
use std::io::{self, BufRead};

#[derive(Deserialize)]
struct Input { accession: u64, metadata: Value }

fn main() -> anyhow::Result<()> {
    for line in io::stdin().lock().lines() {
        let input: Input = serde_json::from_str(&line?)?;
        println!("{}", serde_json::to_string(&flatten_metadata(&input.metadata,input.accession)?)?);
    }
    Ok(())
}
