//! Country-to-DAB-channel mappings.
//!
//! Each entry lists the Band III channel names in use for that country.
//! Sources: ETSI TR 101 496, national frequency coordinators, and
//! public multiplex registries.

/// Returns the DAB Band III channels allocated for a given ISO 3166-1
/// alpha-2 country code (case-insensitive).  Returns `None` for unknown codes.
pub fn channels_for_country(code: &str) -> Option<&'static [&'static str]> {
    match code.to_uppercase().as_str() {
        "NO" => Some(NORWAY),
        "GB" => Some(UK),
        "DE" => Some(GERMANY),
        "NL" => Some(NETHERLANDS),
        "SE" => Some(SWEDEN),
        "DK" => Some(DENMARK),
        "IT" => Some(ITALY),
        "CH" => Some(SWITZERLAND),
        "BE" => Some(BELGIUM),
        "AT" => Some(AUSTRIA),
        "PL" => Some(POLAND),
        _ => None,
    }
}

/// Return a list of all supported countries as `(iso_code, display_name, channels)` triples.
pub fn country_list() -> &'static [(&'static str, &'static str, &'static [&'static str])] {
    COUNTRY_TABLE
}

/// Print a formatted list of all supported countries.
pub fn print_countries() {
    println!("{:<6}  Country", "Code");
    println!("{}", "-".repeat(40));
    for (code, name, channels) in COUNTRY_TABLE {
        println!("{code:<6}  {name:<20}  ({} channels)", channels.len());
    }
}

static COUNTRY_TABLE: &[(&str, &str, &[&str])] = &[
    ("NO", "Norway", NORWAY),
    ("GB", "United Kingdom", UK),
    ("DE", "Germany", GERMANY),
    ("NL", "Netherlands", NETHERLANDS),
    ("SE", "Sweden", SWEDEN),
    ("DK", "Denmark", DENMARK),
    ("IT", "Italy", ITALY),
    ("CH", "Switzerland", SWITZERLAND),
    ("BE", "Belgium", BELGIUM),
    ("AT", "Austria", AUSTRIA),
    ("PL", "Poland", POLAND),
];

// ─── Norway ──────────────────────────────────────────────────────────────── //
//
// Sources: Norkring / Bauer Media / NRK multiplex registrations.
//
// National multiplexes
//   11D  222.064 MHz  – National block 2 (P4, Radio Norge, Radio Rock, …)
//   12A  223.936 MHz  – National block 3 (NRK, NRK P1+, NRK P2, NRK P3, …)
//   12C  227.360 MHz  – National block 4 (commercial: Bauer, NENT, …)
//
// Regional/local multiplexes (selection of the most widely used)
//   5A   174.928 MHz  – Østfold / Viken
//   7B   190.640 MHz  – Telemark
//   7D   194.064 MHz  – Agder
//   8A   195.936 MHz  – Rogaland / Stavanger
//   9A   202.928 MHz  – Hordaland / Bergen
//   9C   206.352 MHz  – Møre og Romsdal
//  10B   211.648 MHz  – Trøndelag / Trondheim
//  11A   216.928 MHz  – Tromsø / Troms
//  12D   229.072 MHz  – Nordland / Bodø
//  13A   230.784 MHz  – Finnmark

static NORWAY: &[&str] = &[
    "5A", "7B", "7D", "8A", "9A", "9C", "10B", "11A", "11D", "12A", "12C", "12D", "13A",
];

// ─── United Kingdom ───────────────────────────────────────────────────────── //
static UK: &[&str] = &[
    "11A", "11B", "11C", "11D", "12A", "12B", "12C", "12D", "10B", "10C", "10D",
];

// ─── Germany ──────────────────────────────────────────────────────────────── //
static GERMANY: &[&str] = &[
    "5C", "7A", "7B", "7C", "7D", "8A", "8B", "8C", "8D", "9A", "9B", "9C", "9D", "10A", "10B",
    "10C", "10D", "11A", "11B", "11C", "11D", "12A", "12C", "12D",
];

// ─── Netherlands ──────────────────────────────────────────────────────────── //
static NETHERLANDS: &[&str] = &["11B", "11C", "12A", "12B", "12C"];

// ─── Sweden ───────────────────────────────────────────────────────────────── //
static SWEDEN: &[&str] = &["10A", "10B", "11A", "11B", "11C", "11D", "12A", "12B"];

// ─── Denmark ──────────────────────────────────────────────────────────────── //
static DENMARK: &[&str] = &["10B", "10C", "11A", "11B", "11C", "12A"];

// ─── Italy ────────────────────────────────────────────────────────────────── //
static ITALY: &[&str] = &[
    "10A", "10B", "10C", "10D", "11A", "11B", "11C", "12A", "12B",
];

// ─── Switzerland ──────────────────────────────────────────────────────────── //
static SWITZERLAND: &[&str] = &["12A", "12B", "12C", "12D", "13A", "13B"];

// ─── Belgium ──────────────────────────────────────────────────────────────── //
static BELGIUM: &[&str] = &["11D", "12A", "12B"];

// ─── Austria ──────────────────────────────────────────────────────────────── //
static AUSTRIA: &[&str] = &["10A", "10B", "10C", "11A", "11B", "12A"];

// ─── Poland ───────────────────────────────────────────────────────────────── //
static POLAND: &[&str] = &["11B", "11C", "12A", "12B", "12C"];

// ─────────────────────────────────────────────────────────────────────────── //
//  Tests                                                                       //
// ─────────────────────────────────────────────────────────────────────────── //

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn norway_channels_non_empty() {
        let ch = channels_for_country("NO").expect("Norway should be defined");
        assert!(!ch.is_empty());
    }

    #[test]
    fn norway_includes_national_multiplexes() {
        let ch = channels_for_country("no").expect("case-insensitive lookup");
        assert!(ch.contains(&"11D"), "missing national block 2 (11D)");
        assert!(ch.contains(&"12A"), "missing NRK national (12A)");
        assert!(ch.contains(&"12C"), "missing commercial national (12C)");
    }

    #[test]
    fn unknown_country_returns_none() {
        assert!(channels_for_country("ZZ").is_none());
    }

    #[test]
    fn all_norwegian_channels_are_valid_band3() {
        use crate::channel_to_freq;
        for &ch in channels_for_country("NO").unwrap() {
            assert!(
                channel_to_freq(ch).is_some(),
                "Norwegian channel {ch} not found in Band III table"
            );
        }
    }

    #[test]
    fn country_table_all_channels_valid() {
        use crate::channel_to_freq;
        for &(code, _, channels) in COUNTRY_TABLE {
            for &ch in channels {
                assert!(
                    channel_to_freq(ch).is_some(),
                    "Country {code}: channel {ch} missing from Band III table"
                );
            }
        }
    }
}
