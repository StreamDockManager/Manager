//! User-initiated address search for the trusted editor. Only postal matches are cached.
use crate::Runtime;
use reqwest::{blocking::Client, redirect::Policy, Url};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    io::Read,
    sync::Mutex,
    time::{Duration, Instant},
};

const PROVIDER_KEY: &str = "location_search_provider";
const OPEN_METEO_SEARCH: &str = "https://geocoding-api.open-meteo.com/v1/search";
const MAX_BODY: u64 = 256 * 1024;
const TTL: Duration = Duration::from_secs(86400);

#[derive(Clone, Debug, Serialize, ts_rs::TS)]
pub struct LocationMatch {
    pub label: String,
    pub latitude: f64,
    pub longitude: f64,
}

fn provider(value: &str) -> Result<Url, String> {
    let url = Url::parse(value.trim()).map_err(|_| "Enter a Nominatim-compatible search URL.")?;
    let host = url.host_str().unwrap_or("").trim_end_matches('.');
    if host.is_empty()
        || host == "openstreetmap.org"
        || host.ends_with(".openstreetmap.org")
        || !(url.scheme() == "https"
            || (url.scheme() == "http" && matches!(host, "127.0.0.1" | "[::1]")))
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || value.len() > 1024
    {
        return Err("Choose a private HTTPS provider or a shared location broker. Local brokers may use loopback HTTP.".into());
    }
    Ok(url)
}

// Select only the fixed built-in endpoint or a validated custom endpoint
#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchProvider {
    OpenMeteo,
    Nominatim,
}

fn search_url(
    endpoint: &str,
    query: &str,
    country: &str,
    postal: bool,
) -> Result<(Url, SearchProvider), String> {
    let query = query.trim();
    let country = country.trim().to_ascii_lowercase();
    if query.is_empty()
        || query.len() > 512
        || query.chars().any(char::is_control)
        || (!country.is_empty()
            && (country.len() != 2 || !country.bytes().all(|c| c.is_ascii_alphabetic())))
        || (postal
            && (country.is_empty()
                || query.len() > 20
                || !query
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b' ' | b'-'))))
    {
        return Err("Check the search text and the two-letter country code. Postal searches require a country.".into());
    }
    let (mut url, source) = if endpoint.is_empty() {
        if query.chars().count() < 2 {
            return Err("Enter at least two characters to search for a place.".into());
        }
        (
            Url::parse(OPEN_METEO_SEARCH).expect("fixed Open-Meteo search URL"),
            SearchProvider::OpenMeteo,
        )
    } else {
        (provider(endpoint)?, SearchProvider::Nominatim)
    };
    let mut pairs = url.query_pairs_mut();
    match source {
        SearchProvider::OpenMeteo => {
            pairs.extend_pairs([
                ("name", query),
                ("count", "5"),
                ("format", "json"),
                ("language", "en"),
            ]);
            if !country.is_empty() {
                pairs.append_pair("countryCode", &country.to_ascii_uppercase());
            }
        }
        SearchProvider::Nominatim => {
            pairs.extend_pairs([
                ("format", "jsonv2"),
                ("limit", "5"),
                ("addressdetails", "1"),
                (if postal { "postalcode" } else { "q" }, query),
            ]);
            if !country.is_empty() {
                pairs.append_pair("countrycodes", &country);
            }
        }
    }
    drop(pairs);
    Ok((url, source))
}

#[derive(Default)]
pub struct LocationSearch(Mutex<SearchState>);
#[derive(Default)]
struct SearchState {
    next: Option<Instant>,
    postal: HashMap<String, (Instant, Vec<LocationMatch>)>,
}

impl LocationSearch {
    pub fn search(
        &self,
        endpoint: &str,
        query: &str,
        country: &str,
        postal: bool,
        cancelled: impl Fn() -> bool,
    ) -> Result<Vec<LocationMatch>, String> {
        let (url, source) = search_url(endpoint, query, country, postal)?;
        let mut state = self
            .0
            .try_lock()
            .map_err(|_| "Another location search is running.")?;
        let now = Instant::now();
        if cancelled() {
            return Err("Location search cancelled.".into());
        }
        state
            .postal
            .retain(|_, (at, _)| now.duration_since(*at) < TTL);
        if postal {
            if let Some((_, results)) = state.postal.get(url.as_str()) {
                return Ok(results.clone());
            }
        }
        if state.next.is_some_and(|next| next > now) {
            return Err("Please wait before searching again.".into());
        }
        state.next = Some(now + Duration::from_millis(1100));
        let client = Client::builder()
            .timeout(Duration::from_secs(8))
            .redirect(Policy::none())
            .user_agent(
                "StreamDockManager location search (+https://github.com/StreamDockManager/Manager)",
            )
            .build()
            .map_err(|_| "Location search is unavailable.")?;
        let response = client
            .get(url.clone())
            .header("Accept", "application/json")
            .send()
            .map_err(|_| "Could not reach the location provider.")?;
        if matches!(response.status().as_u16(), 429 | 503) {
            let delay = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| {
                    v.parse::<u64>().ok().or_else(|| {
                        chrono::DateTime::parse_from_rfc2822(v).ok().map(|at| {
                            (at.timestamp() - chrono::Utc::now().timestamp()).max(1) as u64
                        })
                    })
                })
                .unwrap_or(60)
                .clamp(1, 86400);
            state.next = Some(Instant::now() + Duration::from_secs(delay));
            return Err(format!(
                "Location provider is busy. Try again in {delay} seconds."
            ));
        }
        if !response.status().is_success() {
            return Err(format!(
                "Location provider returned HTTP {}.",
                response.status().as_u16()
            ));
        }
        let mut bytes = Vec::new();
        response
            .take(MAX_BODY + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Location response was unreadable.")?;
        let results = parse_matches(&bytes, source)?;
        if cancelled() {
            return Err("Location search cancelled.".into());
        }
        if postal {
            if state.postal.len() >= 256 {
                if let Some(key) = state
                    .postal
                    .iter()
                    .min_by_key(|(_, v)| v.0)
                    .map(|(k, _)| k.clone())
                {
                    state.postal.remove(&key);
                }
            }
            state
                .postal
                .insert(url.to_string(), (Instant::now(), results.clone()));
        }
        Ok(results)
    }
}

// Keep both providers behind the same response size and coordinate limits
fn parse_matches(bytes: &[u8], source: SearchProvider) -> Result<Vec<LocationMatch>, String> {
    const INVALID: &str = "Invalid location response.";
    if bytes.len() as u64 > MAX_BODY {
        return Err(INVALID.into());
    }

    let make_match = |label: String, latitude: f64, longitude: f64| {
        if !(-90.0..=90.0).contains(&latitude)
            || !(-180.0..=180.0).contains(&longitude)
            || label.trim().is_empty()
            || label.len() > 2048
            || label.chars().any(char::is_control)
        {
            return Err(INVALID.into());
        }
        Ok(LocationMatch {
            label,
            latitude,
            longitude,
        })
    };

    match source {
        SearchProvider::Nominatim => {
            #[derive(Deserialize)]
            struct Row {
                display_name: String,
                lat: String,
                lon: String,
            }
            let rows: Vec<Row> = serde_json::from_slice(bytes).map_err(|_| INVALID)?;
            if rows.len() > 40 {
                return Err(INVALID.into());
            }
            rows.into_iter()
                .take(5)
                .map(|row| {
                    make_match(
                        row.display_name,
                        row.lat.parse().map_err(|_| INVALID)?,
                        row.lon.parse().map_err(|_| INVALID)?,
                    )
                })
                .collect()
        }
        SearchProvider::OpenMeteo => {
            #[derive(Deserialize)]
            struct Envelope {
                #[serde(default)]
                results: Vec<Row>,
                #[serde(default)]
                error: bool,
            }
            #[derive(Deserialize)]
            struct Row {
                name: String,
                latitude: f64,
                longitude: f64,
                admin1: Option<String>,
                country: Option<String>,
            }
            let envelope: Envelope = serde_json::from_slice(bytes).map_err(|_| INVALID)?;
            if envelope.error || envelope.results.len() > 40 {
                return Err(INVALID.into());
            }

            // Qualify matching names without repeating a city that is also its own region
            envelope
                .results
                .into_iter()
                .take(5)
                .map(|row| {
                    if row.name.trim().is_empty() {
                        return Err(INVALID.into());
                    }
                    let mut parts = Vec::<&str>::with_capacity(3);
                    for part in [
                        Some(row.name.as_str()),
                        row.admin1.as_deref(),
                        row.country.as_deref(),
                    ]
                    .into_iter()
                    .flatten()
                    {
                        if part.chars().any(char::is_control) {
                            return Err(INVALID.into());
                        }
                        let part = part.trim();
                        if !part.is_empty()
                            && !parts.iter().any(|other| other.eq_ignore_ascii_case(part))
                        {
                            parts.push(part);
                        }
                    }
                    make_match(parts.join(", "), row.latitude, row.longitude)
                })
                .collect()
        }
    }
}

impl Runtime {
    pub fn location_provider(&self) -> Result<String, String> {
        Ok(self
            .profiles
            .storage
            .read_json::<String>(
                stream_dock_manager_storage::Domain::ManagerPreferences,
                PROVIDER_KEY,
                4096,
            )?
            .unwrap_or_default())
    }
    pub fn set_location_provider(&self, endpoint: &str) -> Result<(), String> {
        let value = if endpoint.trim().is_empty() {
            String::new()
        } else {
            provider(endpoint)?.to_string()
        };
        self.profiles.storage.write_json(
            stream_dock_manager_storage::Domain::ManagerPreferences,
            PROVIDER_KEY,
            &value,
            4096,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn searches_a_local_provider_and_bounds_cache_and_backoff() {
        use std::{
            io::{BufRead, BufReader, Write},
            net::TcpListener,
            thread,
        };
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/search", listener.local_addr().unwrap());
        let worker = thread::spawn(move || {
            for (expected, status, body) in [
                (
                    "postalcode=12345",
                    "200 OK",
                    r#"[{"display_name":"Fixture","lat":"0","lon":"0"}]"#,
                ),
                ("q=private+place", "200 OK", "[]"),
                ("q=busy", "429 Too Many Requests", "[]"),
            ] {
                let (mut socket, _) = listener.accept().unwrap();
                let mut request = String::new();
                BufReader::new(&socket).read_line(&mut request).unwrap();
                assert!(request.contains(expected));
                write!(socket,"HTTP/1.1 {status}\r\nRetry-After: 17\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).unwrap();
            }
        });
        let service = LocationSearch::default();
        let postal = || service.search(&endpoint, "12345", "US", true, || false);
        assert_eq!(postal().unwrap()[0].label, "Fixture");
        assert_eq!(postal().unwrap()[0].longitude, 0.0);
        assert!(service
            .search(&endpoint, "retired", "", false, || true)
            .is_err());
        assert!(service
            .search(&endpoint, "paced", "", false, || false)
            .is_err());
        service.0.lock().unwrap().next = None;
        assert!(service
            .search(&endpoint, "private place", "", false, || false)
            .unwrap()
            .is_empty());
        let mut state = service.0.lock().unwrap();
        assert_eq!(state.postal.len(), 1);
        assert!(!state.postal.keys().any(|key| key.contains("private")));
        state.next = None;
        drop(state);
        assert!(service
            .search(&endpoint, "busy", "", false, || false)
            .unwrap_err()
            .contains("17 seconds"));
        assert!(service
            .search(&endpoint, "paced", "", false, || false)
            .is_err());
        worker.join().unwrap();
    }

    #[test]
    fn validates_destination_query_and_result() {
        for bad in [
            "https://nominatim.openstreetmap.org/search",
            "http://geo.example/search",
            "https://user:pass@geo.example/search",
            "https://geo.example/search?q=a",
        ] {
            assert!(provider(bad).is_err());
        }
        let (url, source) =
            search_url("http://127.0.0.1:8095/search", "AB 12", "GB", true).unwrap();
        assert!(source == SearchProvider::Nominatim);
        assert!(url.as_str().contains("postalcode=AB+12&countrycodes=gb"));
        assert!(search_url(url.as_str(), "test", "", true).is_err());
        assert!(parse_matches(br#"[{"lat":"NaN","lon":"0","display_name":"X"}]"#, source).is_err());
        assert_eq!(
            parse_matches(
                br#"[{"lat":"0","lon":"0","display_name":"Example"}]"#,
                source
            )
            .unwrap()[0]
                .latitude,
            0.0
        );
    }
}

#[cfg(test)]
mod open_meteo_tests {
    use super::*;

    #[test]
    fn default_uses_fixed_endpoint_and_city_or_postal_parameters() {
        for (query, postal) in [("Réunion, Saint-Denis", false), ("AB 12", true)] {
            let (url, source) = search_url("", query, " gB ", postal).unwrap();
            assert!(source == SearchProvider::OpenMeteo);
            assert_eq!(
                url.origin().ascii_serialization(),
                "https://geocoding-api.open-meteo.com"
            );
            assert_eq!(url.path(), "/v1/search");
            let params = url.query_pairs().collect::<HashMap<_, _>>();
            assert_eq!(params.len(), 5);
            for (key, value) in [
                ("name", query),
                ("count", "5"),
                ("format", "json"),
                ("language", "en"),
                ("countryCode", "GB"),
            ] {
                assert_eq!(params[key], value);
            }
        }
        let (url, _) = search_url("", "Berlin", "", false).unwrap();
        assert!(!url.query_pairs().any(|(key, _)| key == "countryCode"));
        assert!(search_url("", "A", "", false).is_err());
        assert!(search_url("", "12345", "", true).is_err());
        assert!(search_url("", "12345&count=100", "US", true).is_err());
    }

    #[test]
    fn parses_only_bounded_open_meteo_locations_and_ignores_unneeded_metadata() {
        let source = SearchProvider::OpenMeteo;
        let rows = parse_matches(
            br#"{"results":[{"name":"Berlin","admin1":"Berlin","country":"Germany","latitude":52.52437,"longitude":13.41053,"timezone":"Europe/Berlin","postcodes":["10115"]},{"name":"Zero","latitude":0,"longitude":0}],"generationtime_ms":1.0}"#,
            source,
        )
        .unwrap();
        assert_eq!(rows[0].label, "Berlin, Germany");
        assert_eq!((rows[0].latitude, rows[0].longitude), (52.52437, 13.41053));
        assert_eq!(rows[1].label, "Zero");
        assert_eq!((rows[1].latitude, rows[1].longitude), (0.0, 0.0));
        assert!(parse_matches(br#"{"generationtime_ms":1.0}"#, source)
            .unwrap()
            .is_empty());
        assert!(parse_matches(br#"{"results":[]}"#, source)
            .unwrap()
            .is_empty());
        let valid = serde_json::json!({"name":"Test","latitude":0,"longitude":0});
        let many = serde_json::json!({"results":vec![valid.clone(); 6]});
        assert_eq!(
            parse_matches(&serde_json::to_vec(&many).unwrap(), source)
                .unwrap()
                .len(),
            5
        );
        let excessive = serde_json::json!({"results":vec![valid; 41]});
        assert!(parse_matches(&serde_json::to_vec(&excessive).unwrap(), source).is_err());
        assert!(parse_matches(&vec![b' '; MAX_BODY as usize + 1], source).is_err());
        for invalid in [
            r#"{"error":true,"reason":"provider detail not returned"}"#,
            r#"{"results":[{"name":"X","latitude":91,"longitude":0}]}"#,
            r#"{"results":[{"name":"X","latitude":0,"longitude":181}]}"#,
            r#"{"results":[{"name":"X","latitude":"0","longitude":0}]}"#,
            r#"{"results":[{"name":"","admin1":"Y","latitude":0,"longitude":0}]}"#,
            r#"{"results":[{"name":"X","country":"Y\nZ","latitude":0,"longitude":0}]}"#,
            r#"[{"display_name":"Wrong provider","lat":"0","lon":"0"}]"#,
        ] {
            assert_eq!(
                parse_matches(invalid.as_bytes(), source).unwrap_err(),
                "Invalid location response."
            );
        }
    }
}
