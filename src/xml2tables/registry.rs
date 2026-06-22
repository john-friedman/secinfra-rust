pub fn supported_xml_document_types() -> &'static [&'static str] {
    &[
        "INFORMATION TABLE",
        "QUALIF",
        "EFFECT",
        "25-NSE",
        "25-NSE/A",
        "C-TR",
        "C-TR-W",
        "X-17A-5",
        "X-17A-5/A",
        "1-K",
        "1-K/A",
        "D",
        "D/A",
        "N-PX",
        "N-PX/A",
        "144",
        "144/A",
        "SCHEDULE 13D",
        "SCHEDULE 13D/A",
        "SCHEDULE 13G",
        "SCHEDULE 13G/A",
        "1-Z",
        "1-Z/A",
        "1-A",
        "1-A/A",
        "1-A POS",
        "DOS",
        "DOS/A",
        "13F-HR",
        "13F-HR/A",
        "13F-NT",
        "13F-NT/A",
        "PROXY VOTING RECORD",
        "COVER PAGE",
        "N-CR",
        "24F-2NT",
        "24F-2NT/A",
        "SH-ER",
        "SDR",
        "SDR/A",
        "CFPORTAL",
        "CFPORTAL/A",
        "CFPORTAL-W",
        "EX-103",
        "EX-102",
        "4",
        "4/A",
        "3",
        "3/A",
        "5",
        "5/A",
        "MA-I",
        "MA-I/A",
        "NPORT-P",
        "NPORT-P/A",
        "TA-2",
        "TA-2/A",
        "N-MFP2",
        "N-MFP2/A",
        "C-U",
        "C-U-W",
        "TA-1",
        "TA-1/A",
        "N-CEN",
        "N-CEN/A",
        "N-MFP",
        "N-MFP/A",
        "MA",
        "MA/A",
        "C/A",
        "C/A-W",
        "MA-A",
        "MA-W",
        "N-MFP1",
        "N-MFP1/A",
        "C-W",
        "C-AR/A",
        "C-AR-W",
        "C",
        "N-MFP3",
        "N-MFP3/A",
        "TA-W",
        "ATS-N/UA",
        "ATS-N-W",
        "ATS-N-C",
        "ATS-N/OFA",
        "ATS-N/CA",
        "ATS-N/MA",
        "C-AR",
        "SBSE-C",
        "SBSE",
        "SBSE/A",
        "SBSE-A",
        "SBSE-A/A",
        "SBSE-BD",
        "SBSE-BD/A",
        "SBSE-W",
        "SBSEF",
        "SBSEF/A",
        "SBSEF-W",
    ]
}

pub fn mapping_json_for_document_type(document_type: &str) -> Option<&'static str> {
    match document_type {
        "INFORMATION TABLE" => Some(include_str!(
            "../../assets/xml_mapping_jsons/informationtable.json"
        )),
        "QUALIF" => Some(include_str!("../../assets/xml_mapping_jsons/qualif.json")),
        "EFFECT" => Some(include_str!("../../assets/xml_mapping_jsons/effect.json")),
        "25-NSE" | "25-NSE/A" => Some(include_str!("../../assets/xml_mapping_jsons/25nse.json")),
        "C-TR" | "C-TR-W" => Some(include_str!("../../assets/xml_mapping_jsons/ctr.json")),
        "X-17A-5" | "X-17A-5/A" => Some(include_str!("../../assets/xml_mapping_jsons/x17a5.json")),
        "1-K" | "1-K/A" => Some(include_str!("../../assets/xml_mapping_jsons/1k.json")),
        "D" | "D/A" => Some(include_str!("../../assets/xml_mapping_jsons/d.json")),
        "N-PX" | "N-PX/A" => Some(include_str!("../../assets/xml_mapping_jsons/npx.json")),
        "144" | "144/A" => Some(include_str!("../../assets/xml_mapping_jsons/144.json")),
        "SCHEDULE 13D" | "SCHEDULE 13D/A" => Some(include_str!(
            "../../assets/xml_mapping_jsons/schedule13d.json"
        )),
        "SCHEDULE 13G" | "SCHEDULE 13G/A" => Some(include_str!(
            "../../assets/xml_mapping_jsons/schedule13g.json"
        )),
        "1-Z" | "1-Z/A" => Some(include_str!("../../assets/xml_mapping_jsons/1z.json")),
        "1-A" | "1-A/A" | "1-A POS" => Some(include_str!("../../assets/xml_mapping_jsons/1a.json")),
        "DOS" | "DOS/A" => Some(include_str!("../../assets/xml_mapping_jsons/dos.json")),
        "13F-HR" | "13F-HR/A" | "13F-NT" | "13F-NT/A" => {
            Some(include_str!("../../assets/xml_mapping_jsons/13fhr.json"))
        }
        "PROXY VOTING RECORD" => Some(include_str!(
            "../../assets/xml_mapping_jsons/proxyvotingrecord.json"
        )),
        "COVER PAGE" => Some(include_str!(
            "../../assets/xml_mapping_jsons/coverpage.json"
        )),
        "N-CR" => Some(include_str!("../../assets/xml_mapping_jsons/ncr.json")),
        "24F-2NT" | "24F-2NT/A" => Some(include_str!("../../assets/xml_mapping_jsons/24f2nt.json")),
        "SH-ER" => Some(include_str!("../../assets/xml_mapping_jsons/sher.json")),
        "SDR" | "SDR/A" => Some(include_str!("../../assets/xml_mapping_jsons/sdr.json")),
        "CFPORTAL" | "CFPORTAL/A" | "CFPORTAL-W" => {
            Some(include_str!("../../assets/xml_mapping_jsons/cfportal.json"))
        }
        "EX-103" => Some(include_str!("../../assets/xml_mapping_jsons/ex103.json")),
        "EX-102" => Some(include_str!("../../assets/xml_mapping_jsons/ex102.json")),
        "4" | "4/A" | "3" | "3/A" | "5" | "5/A" => {
            Some(include_str!("../../assets/xml_mapping_jsons/345.json"))
        }
        "MA-I" | "MA-I/A" => Some(include_str!("../../assets/xml_mapping_jsons/mai.json")),
        "NPORT-P" | "NPORT-P/A" => Some(include_str!("../../assets/xml_mapping_jsons/nportp.json")),
        "TA-2" | "TA-2/A" => Some(include_str!("../../assets/xml_mapping_jsons/ta2.json")),
        "N-MFP2" | "N-MFP2/A" | "N-MFP1" | "N-MFP1/A" => {
            Some(include_str!("../../assets/xml_mapping_jsons/nmfp12.json"))
        }
        "C-U" | "C-U-W" | "C/A" | "C/A-W" | "C-W" | "C-AR/A" | "C-AR-W" | "C" | "C-AR" => {
            Some(include_str!("../../assets/xml_mapping_jsons/c.json"))
        }
        "TA-1" | "TA-1/A" => Some(include_str!("../../assets/xml_mapping_jsons/ta1.json")),
        "N-CEN" | "N-CEN/A" => Some(include_str!("../../assets/xml_mapping_jsons/ncen.json")),
        "N-MFP" | "N-MFP/A" => Some(include_str!("../../assets/xml_mapping_jsons/nmfp.json")),
        "MA" | "MA/A" | "MA-A" => Some(include_str!("../../assets/xml_mapping_jsons/ma.json")),
        "MA-W" => Some(include_str!("../../assets/xml_mapping_jsons/maw.json")),
        "N-MFP3" | "N-MFP3/A" => Some(include_str!("../../assets/xml_mapping_jsons/nmfp3.json")),
        "TA-W" => Some(include_str!("../../assets/xml_mapping_jsons/taw.json")),
        "ATS-N/UA" | "ATS-N-W" | "ATS-N-C" | "ATS-N/OFA" | "ATS-N/CA" | "ATS-N/MA" => {
            Some(include_str!("../../assets/xml_mapping_jsons/atsn_ua.json"))
        }
        "SBSE-C" | "SBSE" | "SBSE/A" | "SBSE-A" | "SBSE-A/A" | "SBSE-BD" | "SBSE-BD/A"
        | "SBSE-W" | "SBSEF" | "SBSEF/A" | "SBSEF-W" => {
            Some(include_str!("../../assets/xml_mapping_jsons/sbsec.json"))
        }
        _ => None,
    }
}
