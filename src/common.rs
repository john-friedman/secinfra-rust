use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SubmissionSource {
    Rss,
    Efts,
}

/// A single SEC filing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Submission {
    pub accession: u64,
    pub submission_type: String,
    pub ciks: Vec<u64>,
    pub filing_date: String,
    pub source: SubmissionSource,
    pub detected_time: DateTime<Utc>,
}

pub fn sec_user_agent() -> String {
    std::env::var("SEC_USER_AGENT").unwrap_or_else(|_| "John Smith johnsmith@gmail.com".to_string())
}
