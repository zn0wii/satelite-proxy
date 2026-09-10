//! Exit-IP probe for the dashboard "network probe" card: races a handful of
//! public IP-info endpoints and returns the first JSON answer (ip +
//! country code), FlClash-style. When the core is running the requests go
//! through its local mixed inbound, so the answer IS the current exit;
//! otherwise they go direct and report the machine's own public IP.
//!
//! Blocking ureq calls live in `spawn_blocking` (see `api/clash_api.rs` for
//! why ureq must stay off the async runtime); losers of the race are simply
//! abandoned — their ureq timeouts (connect 5s / total 9s) bound the waste.

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitIpInfo {
    pub ip: String,
    pub country_code: Option<String>,
    /// Answer fetched through the core's mixed inbound (false = probed
    /// direct: core stopped or Direct outbound mode).
    pub via_proxy: bool,
    /// Which endpoint won the race (URL, for diagnostics).
    pub source: String,
}

/// One candidate endpoint: URL plus the JSON field names it answers with.
#[derive(Clone, Copy)]
struct Source {
    url: &'static str,
    ip_field: &'static str,
    country_field: &'static str,
}

const SOURCES: &[Source] = &[
    // Same endpoints chain_diag trusts for exit verification.
    Source {
        url: "https://api.ip.sb/geoip",
        ip_field: "ip",
        country_field: "country_code",
    },
    Source {
        url: "https://ipwho.is/",
        ip_field: "ip",
        country_field: "country_code",
    },
    Source {
        url: "http://ip-api.com/json",
        ip_field: "query",
        country_field: "countryCode",
    },
    Source {
        url: "https://api.myip.com",
        ip_field: "ip",
        country_field: "cc",
    },
];

/// Plain-browser UA: some endpoints (ip.sb in particular) 403 bare clients.
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// Hard cap on the whole race, regardless of individual ureq timeouts.
const RACE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(12);

/// Fetch (ip, country_code) from one endpoint, through `proxy_port` when
/// given, direct otherwise.
fn probe_source(
    source: &Source,
    proxy_port: Option<u16>,
) -> std::result::Result<(String, Option<String>), String> {
    let mut builder = ureq::AgentBuilder::new()
        .timeout_connect(std::time::Duration::from_secs(5))
        .timeout(std::time::Duration::from_secs(9));
    if let Some(port) = proxy_port {
        let proxy = ureq::Proxy::new(format!("http://127.0.0.1:{port}"))
            .map_err(|e| format!("proxy: {e}"))?;
        builder = builder.proxy(proxy);
    }
    let resp = builder
        .build()
        .get(source.url)
        .set("User-Agent", USER_AGENT)
        .set("Accept", "application/json")
        .call()
        .map_err(|e| format!("{}: {e}", source.url))?;
    let body = resp.into_string().map_err(|e| format!("body: {e}"))?;
    parse_source(source, &body).ok_or_else(|| format!("{}: unparsable answer", source.url))
}

/// Normalize one endpoint's JSON body into (ip, country code). `None` when
/// the body lacks a usable ip (e.g. ipwho.is answers `success:false`).
fn parse_source(source: &Source, body: &str) -> Option<(String, Option<String>)> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let ip = value.get(source.ip_field)?.as_str()?.trim().to_string();
    if ip.is_empty() {
        return None;
    }
    let country = value
        .get(source.country_field)
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    Some((ip, country))
}

/// Race all [`SOURCES`] concurrently; first successful answer wins and the
/// losers are left to die on their own timeouts.
pub async fn probe(mixed_port: u16, via_proxy: bool) -> std::result::Result<ExitIpInfo, String> {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    for source in SOURCES {
        let tx = tx.clone();
        tokio::task::spawn_blocking(move || {
            let result = probe_source(source, via_proxy.then_some(mixed_port));
            let _ = tx.send((source.url, result));
        });
    }
    drop(tx);

    let started = std::time::Instant::now();
    let mut last_error = "all exit-ip sources failed".to_string();
    loop {
        let remaining = RACE_TIMEOUT.saturating_sub(started.elapsed());
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Some((url, Ok((ip, country))))) => {
                return Ok(ExitIpInfo {
                    ip,
                    country_code: country,
                    via_proxy,
                    source: url.to_string(),
                });
            }
            Ok(Some((_, Err(e)))) => last_error = e,
            // All senders done without a success, or the cap elapsed.
            Ok(None) | Err(_) => return Err(last_error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn src(url: &str) -> Source {
        *SOURCES.iter().find(|s| s.url == url).unwrap()
    }

    #[test]
    fn parses_ip_sb_geoip() {
        let (ip, cc) = parse_source(
            &src("https://api.ip.sb/geoip"),
            r#"{"ip":"104.28.7.9","country_code":"US","country":"United States"}"#,
        )
        .unwrap();
        assert_eq!(ip, "104.28.7.9");
        assert_eq!(cc.as_deref(), Some("US"));
    }

    #[test]
    fn parses_ipwho_is() {
        let (ip, cc) = parse_source(
            &src("https://ipwho.is/"),
            r#"{"ip":"1.2.3.4","success":true,"country_code":"JP"}"#,
        )
        .unwrap();
        assert_eq!(ip, "1.2.3.4");
        assert_eq!(cc.as_deref(), Some("JP"));
    }

    #[test]
    fn parses_ip_api_com() {
        let (ip, cc) = parse_source(
            &src("http://ip-api.com/json"),
            r#"{"query":"5.6.7.8","countryCode":"DE","country":"Germany"}"#,
        )
        .unwrap();
        assert_eq!(ip, "5.6.7.8");
        assert_eq!(cc.as_deref(), Some("DE"));
    }

    #[test]
    fn parses_myip_com() {
        let (ip, cc) = parse_source(
            &src("https://api.myip.com"),
            r#"{"ip":"9.9.9.9","cc":"SG"}"#,
        )
        .unwrap();
        assert_eq!(ip, "9.9.9.9");
        assert_eq!(cc.as_deref(), Some("SG"));
    }

    #[test]
    fn rejects_failure_shapes() {
        // ipwho.is answers success:false with a null ip.
        assert!(parse_source(
            &src("https://ipwho.is/"),
            r#"{"ip":null,"success":false,"message":"Invalid IP"}"#,
        )
        .is_none());
        // Empty / missing ip fields.
        assert!(parse_source(&src("https://api.myip.com"), r#"{"ip":"","cc":"US"}"#).is_none());
        assert!(parse_source(&src("https://api.myip.com"), "not json").is_none());
        // Country code is optional — ip alone still parses.
        let (ip, cc) = parse_source(&src("https://api.myip.com"), r#"{"ip":"1.1.1.1"}"#).unwrap();
        assert_eq!(ip, "1.1.1.1");
        assert!(cc.is_none());
    }
}
