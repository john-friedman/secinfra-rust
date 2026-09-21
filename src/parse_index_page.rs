use std::collections::HashSet;

use anyhow::{Context, Result, bail};
use chrono::{NaiveDate, NaiveDateTime};
use indexmap::IndexMap;
use scraper::{ElementRef, Html, Selector};
use serde::{Deserialize, Serialize};

const MAX_CIK: u64 = 9_999_999_999;

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexPage {
    pub accession: u64,
    pub submission_type: String,
    pub form_description: Option<String>,
    pub filing_date: Option<NaiveDate>,
    pub accepted_at: Option<NaiveDateTime>,
    pub document_count: Option<u32>,
    pub header_metadata: IndexMap<String, String>,
    pub filers: Vec<IndexPageFiler>,
    pub documents: Vec<IndexPageDocument>,
}

impl IndexPage {
    pub fn ciks(&self) -> Vec<u64> {
        let mut seen = HashSet::new();
        self.filers
            .iter()
            .filter_map(|filer| filer.cik)
            .filter(|cik| seen.insert(*cik))
            .collect()
    }
}

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexPageFiler {
    pub name: String,
    pub role: Option<String>,
    pub cik: Option<u64>,
    pub mailing_address: Option<String>,
    pub business_address: Option<String>,
    pub identification: Option<String>,
}

#[non_exhaustive]
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexPageDocument {
    pub sequence: Option<u32>,
    pub description: Option<String>,
    pub filename: Option<String>,
    pub href: Option<String>,
    pub document_type: Option<String>,
    pub size_bytes: Option<u64>,
}

pub fn parse_index_page(html: &[u8]) -> Result<IndexPage> {
    let html = std::str::from_utf8(html).context("SEC index page was not UTF-8")?;
    let document = Html::parse_document(html);

    let form_name_selector = selector("#formName")?;
    let strong_selector = selector("strong")?;
    let form_name = document
        .select(&form_name_selector)
        .next()
        .context("SEC index page did not contain #formName")?;
    let form_header = form_name
        .select(&strong_selector)
        .next()
        .context("SEC index page form header did not contain strong text")?;
    let form_header = element_text(form_header);
    let submission_type = form_header
        .strip_prefix("Form ")
        .unwrap_or(&form_header)
        .trim()
        .to_owned();
    if submission_type.is_empty() {
        bail!("SEC index page contained an empty submission type");
    }
    let form_name_text = element_text(form_name);
    let form_description = nonempty(
        form_name_text
            .strip_prefix(&form_header)
            .unwrap_or_default()
            .trim()
            .trim_start_matches('-')
            .trim()
            .trim_end_matches(':')
            .trim()
            .to_owned(),
    );

    let sec_num_selector = selector("#secNum")?;
    let sec_num = document
        .select(&sec_num_selector)
        .next()
        .map(element_text)
        .context("SEC index page did not contain #secNum")?;
    let accession = sec_num
        .split_whitespace()
        .find(|value| value.bytes().filter(|byte| byte.is_ascii_digit()).count() == 18)
        .map(|value| {
            value
                .chars()
                .filter(char::is_ascii_digit)
                .collect::<String>()
        })
        .and_then(|value| value.parse::<u64>().ok())
        .context("SEC index page did not contain a valid accession")?;

    let mut header_metadata = IndexMap::new();
    let head_selector = selector(".formGrouping .infoHead")?;
    let info_selector = selector(".formGrouping .info")?;
    for (head, value) in document
        .select(&head_selector)
        .zip(document.select(&info_selector))
    {
        let head = element_text(head);
        let value = element_text(value);
        if !head.is_empty() && !value.is_empty() {
            header_metadata.insert(head, value);
        }
    }
    let filing_date = header_metadata
        .get("Filing Date")
        .and_then(|value| NaiveDate::parse_from_str(value, "%Y-%m-%d").ok());
    let accepted_at = header_metadata
        .get("Accepted")
        .and_then(|value| NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S").ok());
    let document_count = header_metadata
        .get("Documents")
        .and_then(|value| value.parse::<u32>().ok());

    let filers = parse_filers(&document)?;
    let documents = parse_documents(&document)?;

    Ok(IndexPage {
        accession,
        submission_type,
        form_description,
        filing_date,
        accepted_at,
        document_count,
        header_metadata,
        filers,
        documents,
    })
}

fn parse_filers(document: &Html) -> Result<Vec<IndexPageFiler>> {
    let filer_selector = selector(".filerDiv")?;
    let company_selector = selector(".companyName")?;
    let link_selector = selector("a[href]")?;
    let mailer_selector = selector(".mailer")?;
    let ident_selector = selector(".identInfo")?;
    let mut filers = Vec::new();

    for filer in document.select(&filer_selector) {
        let Some(company) = filer.select(&company_selector).next() else {
            continue;
        };
        let company_text = element_text(company);
        let identity = company_text
            .split("CIK")
            .next()
            .unwrap_or(&company_text)
            .trim()
            .to_owned();
        let (name, role) = split_name_and_role(&identity);
        let cik = company
            .select(&link_selector)
            .filter_map(|link| link.value().attr("href"))
            .filter_map(cik_from_href)
            .find(|cik| (1..=MAX_CIK).contains(cik));

        let mut mailing_address = None;
        let mut business_address = None;
        for mailer in filer.select(&mailer_selector) {
            let value = element_text(mailer);
            if let Some(address) = value.strip_prefix("Mailing Address") {
                mailing_address = nonempty(address.trim().to_owned());
            } else if let Some(address) = value.strip_prefix("Business Address") {
                business_address = nonempty(address.trim().to_owned());
            }
        }

        filers.push(IndexPageFiler {
            name,
            role,
            cik,
            mailing_address,
            business_address,
            identification: filer
                .select(&ident_selector)
                .next()
                .map(element_text)
                .and_then(nonempty),
        });
    }

    Ok(filers)
}

fn parse_documents(document: &Html) -> Result<Vec<IndexPageDocument>> {
    let row_selector = selector("table.tableFile tr")?;
    let cell_selector = selector("td")?;
    let link_selector = selector("a[href]")?;
    let mut documents = Vec::new();

    for row in document.select(&row_selector) {
        let cells = row.select(&cell_selector).collect::<Vec<_>>();
        if cells.len() < 5 {
            continue;
        }
        let link = cells[2].select(&link_selector).next();
        documents.push(IndexPageDocument {
            sequence: parse_optional_u32(&element_text(cells[0])),
            description: nonempty(element_text(cells[1])),
            filename: nonempty(element_text(cells[2])),
            href: link
                .and_then(|link| link.value().attr("href"))
                .map(str::to_owned),
            document_type: nonempty(element_text(cells[3])),
            size_bytes: parse_optional_u64(&element_text(cells[4])),
        });
    }

    Ok(documents)
}

fn split_name_and_role(value: &str) -> (String, Option<String>) {
    let value = value.trim();
    if let Some(open) = value.rfind('(')
        && value.ends_with(')')
    {
        let role = value[open + 1..value.len() - 1].trim();
        let name = value[..open].trim();
        if !name.is_empty() && !role.is_empty() {
            return (name.to_owned(), Some(role.to_owned()));
        }
    }
    (value.to_owned(), None)
}

fn cik_from_href(href: &str) -> Option<u64> {
    let query = href.split_once('?')?.1;
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        key.eq_ignore_ascii_case("CIK")
            .then(|| value.parse::<u64>().ok())
            .flatten()
    })
}

fn selector(value: &str) -> Result<Selector> {
    Selector::parse(value).map_err(|_| anyhow::anyhow!("invalid internal selector {value}"))
}

fn element_text(element: ElementRef<'_>) -> String {
    normalize_text(element.text())
}

fn normalize_text<'a>(parts: impl Iterator<Item = &'a str>) -> String {
    parts
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

fn nonempty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

fn parse_optional_u32(value: &str) -> Option<u32> {
    value.replace(',', "").parse().ok()
}

fn parse_optional_u64(value: &str) -> Option<u64> {
    value.replace(',', "").parse().ok()
}