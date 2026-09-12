use crate::domain::ManualNodeDraft;
use crate::domain::{
    ParseResult, ProxyNode, Subscription, SubscriptionFormat, SubscriptionSource,
    SubscriptionTraffic,
};
use crate::error::{AppError, AppResult};
use crate::subscription::{
    parse_manual_draft, parse_single_uri, parse_subscription, validate_complete_singbox_config,
};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_BODY_BYTES: usize = 8 * 1024 * 1024;

/// Conservative comparison key for subscription URLs. Query order and path case are
/// intentionally preserved because they can carry signed credentials.
pub(crate) fn canonical_subscription_url(input: &str) -> Option<String> {
    let mut url = url::Url::parse(input.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    if (url.scheme() == "http" && url.port() == Some(80))
        || (url.scheme() == "https" && url.port() == Some(443))
    {
        let _ = url.set_port(None);
    }
    url.set_fragment(None);
    if url.path().is_empty() {
        url.set_path("/");
    }
    Some(url.to_string())
}

pub struct ImportOutcome {
    pub subscription: Subscription,
    pub nodes: Vec<ProxyNode>,
    /// Nodes the parser could not import, with reasons — surfaced to the UI
    /// so the user can report unsupported protocols/transports to the dev.
    pub skipped: Vec<crate::domain::SkippedProxy>,
}

/// `via_proxy`: fetch through local mixed HTTP proxy (127.0.0.1:mixed_port).
/// `mixed_port`: required when via_proxy is true.
/// `user_agent`: custom UA override; empty/None falls back to the built-in default.
pub async fn import_from_url_with_id(
    name: Option<String>,
    url: String,
    existing_id: Option<String>,
    via_proxy: bool,
    mixed_port: Option<u16>,
    user_agent: Option<String>,
) -> AppResult<ImportOutcome> {
    let url = url.trim().to_string();
    if url.is_empty() {
        return Err(AppError::Fetch("url is empty".into()));
    }
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(AppError::Fetch(
            "url must start with http:// or https://".into(),
        ));
    }

    let custom_ua = user_agent
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string);

    // Many panels only attach `subscription-userinfo` when UA looks like Clash;
    // some also substring-whitelist `clash-verge` or `flclash`. See
    // `subscription_user_agent` for the exact shape.
    let ua = custom_ua.clone().unwrap_or_else(subscription_user_agent);

    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .redirect(reqwest::redirect::Policy::limited(10))
        .user_agent(ua);

    if via_proxy {
        let port = mixed_port.unwrap_or(2080);
        let proxy_url = format!("http://127.0.0.1:{port}");
        let proxy = reqwest::Proxy::all(&proxy_url)
            .map_err(|e| AppError::Fetch(format!("invalid proxy {proxy_url}: {e}")))?;
        builder = builder.proxy(proxy);
    } else {
        builder = builder.no_proxy();
    }

    let client = builder
        .build()
        .map_err(|e| AppError::Fetch(e.to_string()))?;

    let response = client
        .get(&url)
        .header(reqwest::header::ACCEPT, "*/*")
        .send()
        .await
        .map_err(|e| {
            if via_proxy {
                AppError::Fetch(format!(
                    "via proxy failed ({e}). 请确认已启动代理核心，且 mixed 端口正确"
                ))
            } else {
                AppError::Fetch(e.to_string())
            }
        })?;

    if !response.status().is_success() {
        return Err(AppError::Fetch(format!(
            "http status {}",
            response.status()
        )));
    }

    let traffic = parse_subscription_userinfo(response.headers());
    // Default label from Content-Disposition (RFC 5987 filename*), same as FlClash.
    let disposition_name = parse_content_disposition_filename(response.headers());

    let bytes = crate::services::http_body::read_limited(
        response,
        MAX_BODY_BYTES,
        format!("body too large (max {MAX_BODY_BYTES} bytes)"),
    )
    .await
    .map_err(|e| AppError::Fetch(e.to_string()))?;

    // Name priority: user input > Content-Disposition filename* > URL host
    let display_name = name
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .or(disposition_name)
        .unwrap_or_else(|| name_from_url(&url));
    let content = String::from_utf8_lossy(&bytes).into_owned();
    let mut outcome = tokio::task::spawn_blocking(move || -> AppResult<ImportOutcome> {
        let body_traffic = parse_userinfo_from_content(&content);
        let parsed = parse_subscription(&content)?;
        let mut outcome = build_outcome(
            display_name,
            SubscriptionSource::Url { url },
            parsed,
            existing_id,
            true,
        );
        // URL body comment > remark nodes; HTTP headers are merged below.
        outcome.subscription.traffic =
            SubscriptionTraffic::merge(body_traffic, outcome.subscription.traffic);
        Ok(outcome)
    })
    .await
    .map_err(|error| AppError::Fetch(format!("subscription parse task: {error}")))??;
    outcome.subscription.via_proxy = via_proxy;
    outcome.subscription.user_agent = custom_ua;
    // Priority: HTTP header > body comment > remark node names
    outcome.subscription.traffic =
        SubscriptionTraffic::merge(traffic, outcome.subscription.traffic);
    Ok(outcome)
}

/// UA used when fetching subscriptions (must look like Clash for many panels).
fn subscription_user_agent() -> String {
    // FlClash shape plus the verbatim `flclash/1` token: covers panels that
    // substring-match either `clash-verge` or `flclash` in the UA.
    "satelite-proxy/0.1 clash-verge/v2.5 flclash/1".to_string()
}

/// Parse `Content-Disposition` for a display name.
/// Supports `filename*=UTF-8''%E8%89%AF%E5%BF%83%E4%BA%91` (percent-encoded) and
/// plain `filename="foo.yaml"`. Matches FlClash `getFileNameForDisposition`.
pub fn parse_content_disposition_filename(headers: &reqwest::header::HeaderMap) -> Option<String> {
    let raw = headers
        .get(reqwest::header::CONTENT_DISPOSITION)
        .or_else(|| headers.get("content-disposition"))
        .and_then(header_value_to_string)?;
    parse_disposition_filename_str(&raw)
}

fn parse_disposition_filename_str(disposition: &str) -> Option<String> {
    // Prefer RFC 5987: filename*=charset'lang'value  or  filename*=UTF-8''urlencoded
    if let Some(star) = find_disposition_param(disposition, "filename*") {
        let decoded = decode_filename_star(&star)?;
        let cleaned = clean_disposition_name(&decoded);
        if !cleaned.is_empty() {
            return Some(cleaned);
        }
    }
    // Fallback: filename="..." / filename=...
    if let Some(plain) = find_disposition_param(disposition, "filename") {
        let unquoted = plain
            .trim()
            .trim_matches(|c| c == '"' || c == '\'')
            .to_string();
        // Some servers still percent-encode plain filename
        let decoded = urlencoding::decode(&unquoted)
            .map(|c| c.into_owned())
            .unwrap_or(unquoted);
        let cleaned = clean_disposition_name(&decoded);
        if !cleaned.is_empty() {
            return Some(cleaned);
        }
    }
    None
}

/// Extract parameter value from Content-Disposition; handles `filename*=...` vs `filename=...`.
fn find_disposition_param(disposition: &str, key: &str) -> Option<String> {
    let lower_key = key.to_ascii_lowercase();
    for part in disposition.split(';') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        if k.trim().eq_ignore_ascii_case(&lower_key) {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// Decode `UTF-8''%E8%89%AF...` or bare percent-encoded string.
fn decode_filename_star(raw: &str) -> Option<String> {
    let raw = raw.trim().trim_matches(|c| c == '"' || c == '\'');
    // charset'lang'value  — lang often empty: UTF-8''xxx
    let value = if let Some(idx) = raw.find("''") {
        &raw[idx + 2..]
    } else if let Some((_, rest)) = raw.split_once('\'') {
        // charset'value without empty lang
        rest.trim_start_matches('\'')
    } else {
        raw
    };
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    // RFC 5987 uses percent-encoding
    match urlencoding::decode(value) {
        Ok(cow) => {
            let s = cow.into_owned();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        }
        Err(_) => {
            // Fallback: try percent_decode via from_utf8 lossy
            let bytes: Vec<u8> = {
                let mut out = Vec::new();
                let b = value.as_bytes();
                let mut i = 0;
                while i < b.len() {
                    if b[i] == b'%' && i + 2 < b.len() {
                        let h = std::str::from_utf8(&b[i + 1..i + 3]).ok();
                        if let Some(h) = h {
                            if let Ok(n) = u8::from_str_radix(h, 16) {
                                out.push(n);
                                i += 3;
                                continue;
                            }
                        }
                    }
                    out.push(b[i]);
                    i += 1;
                }
                out
            };
            let s = String::from_utf8_lossy(&bytes).into_owned();
            if s.is_empty() {
                None
            } else {
                Some(s)
            }
        }
    }
}

fn clean_disposition_name(name: &str) -> String {
    let mut s = name.trim().to_string();
    // Drop path components if any
    if let Some(base) = s.rsplit(['/', '\\']).next() {
        s = base.to_string();
    }
    // Strip common subscription file extensions for a nicer label
    for ext in [".yaml", ".yml", ".txt", ".conf", ".json"] {
        if s.to_ascii_lowercase().ends_with(ext) {
            s.truncate(s.len() - ext.len());
            break;
        }
    }
    s.trim().to_string()
}

/// Parse Clash-style `subscription-userinfo` header:
/// `upload=…; download=…; total=…; expire=…` (values in bytes / unix seconds).
pub fn parse_subscription_userinfo(
    headers: &reqwest::header::HeaderMap,
) -> Option<SubscriptionTraffic> {
    // 1) Standard name (HeaderMap is case-insensitive).
    if let Some(raw) = header_values_joined(headers, "subscription-userinfo") {
        if let Some(t) = parse_userinfo_str(&raw) {
            return Some(t);
        }
    }
    // 2) Some panels use slightly different names.
    for name in [
        "subscription-userinfo",
        "x-subscription-userinfo",
        "subscription-user-info",
    ] {
        if let Some(raw) = header_values_joined(headers, name) {
            if let Some(t) = parse_userinfo_str(&raw) {
                return Some(t);
            }
        }
    }
    // 3) Scan any header whose name contains "userinfo".
    for (name, value) in headers.iter() {
        let n = name.as_str().to_ascii_lowercase();
        if !n.contains("userinfo") {
            continue;
        }
        if let Some(raw) = header_value_to_string(value) {
            if let Some(t) = parse_userinfo_str(&raw) {
                return Some(t);
            }
        }
    }
    None
}

fn header_values_joined(headers: &reqwest::header::HeaderMap, name: &str) -> Option<String> {
    let values: Vec<String> = headers
        .get_all(name)
        .iter()
        .filter_map(header_value_to_string)
        .collect();
    if values.is_empty() {
        None
    } else {
        Some(values.join("; "))
    }
}

fn header_value_to_string(v: &reqwest::header::HeaderValue) -> Option<String> {
    if let Ok(s) = v.to_str() {
        let s = s.trim();
        if s.is_empty() {
            return None;
        }
        return Some(s.to_string());
    }
    // Non-strict ASCII: still try (some middleboxes corrupt encoding).
    let s = String::from_utf8_lossy(v.as_bytes());
    let s = s.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

/// Some providers put the same key=value list in a leading YAML comment:
/// `# upload=…; download=…; total=…; expire=…`
pub fn parse_userinfo_from_content(content: &str) -> Option<SubscriptionTraffic> {
    for line in content.lines().take(32) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let body = match line.strip_prefix('#') {
            Some(rest) => rest.trim().trim_end_matches(';').trim(),
            None => {
                // Stop after first non-comment content line (except pure whitespace already skipped).
                // Still allow a few leading blank/comment-only lines above.
                if line.starts_with("proxies")
                    || line.starts_with("port:")
                    || line.starts_with("mixed-port")
                    || line.starts_with("---")
                {
                    break;
                }
                continue;
            }
        };
        if body.is_empty() {
            continue;
        }
        let lower = body.to_ascii_lowercase();
        if !(lower.contains("upload=")
            || lower.contains("download=")
            || lower.contains("total=")
            || lower.contains("expire="))
        {
            continue;
        }
        if let Some(t) = parse_userinfo_str(body) {
            return Some(t);
        }
    }
    None
}

fn parse_userinfo_str(raw: &str) -> Option<SubscriptionTraffic> {
    let mut traffic = SubscriptionTraffic::default();
    // FlClash: split by `;`, then `key=value` (also tolerate commas).
    for part in raw.split([';', ',']) {
        let part = part.trim().trim_end_matches(';');
        if part.is_empty() {
            continue;
        }
        let (k, v) = match part.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        let key = k.trim().to_ascii_lowercase();
        // Tolerate spaces / quotes: " 1073741824000 "
        let val = v
            .trim()
            .trim_matches(|c: char| c == '"' || c == '\'')
            .trim();
        // Some panels send floats as strings — take integer part.
        let parse_u64 = |s: &str| -> Option<u64> {
            if let Ok(n) = s.parse::<u64>() {
                return Some(n);
            }
            s.parse::<f64>().ok().map(|f| f.max(0.0).round() as u64)
        };
        let parse_i64 = |s: &str| -> Option<i64> {
            if let Ok(n) = s.parse::<i64>() {
                return Some(n);
            }
            s.parse::<f64>().ok().map(|f| f as i64)
        };
        match key.as_str() {
            "upload" => traffic.upload = parse_u64(val),
            "download" => traffic.download = parse_u64(val),
            "total" => traffic.total = parse_u64(val),
            "expire" => traffic.expire = parse_i64(val),
            _ => {}
        }
    }
    if traffic.is_empty() {
        None
    } else {
        Some(traffic)
    }
}

/// Providers often inject fake proxies whose **names** carry quota text, e.g.
/// `剩余流量：2.41 TB` / `套餐到期：长期有效`. Extract traffic and drop them.
fn is_remark_name(name: &str) -> bool {
    let mut traffic = SubscriptionTraffic::default();
    apply_remark_name(name, &mut traffic)
}

fn split_remark_nodes(nodes: Vec<ProxyNode>) -> (Option<SubscriptionTraffic>, Vec<ProxyNode>) {
    let mut traffic = SubscriptionTraffic::default();
    let mut real = Vec::with_capacity(nodes.len());
    for n in nodes {
        if apply_remark_name(&n.name, &mut traffic) {
            continue;
        }
        real.push(n);
    }
    let traffic = if traffic.is_empty() {
        None
    } else {
        Some(traffic)
    };
    (traffic, real)
}

/// Returns true if `name` is a remark / info node (should not be a real proxy).
fn apply_remark_name(name: &str, traffic: &mut SubscriptionTraffic) -> bool {
    let name = name.trim();
    if name.is_empty() {
        return false;
    }

    // 剩余流量：2.41 TB / 流量剩余: 100GB / 余量：…
    if let Some(rest) = strip_label(
        name,
        &[
            "剩余流量",
            "流量剩余",
            "剩余额度",
            "余量",
            "流量余量",
            "剩余",
        ],
    ) {
        if let Some(bytes) = parse_size_to_bytes(rest) {
            if traffic.quota_remaining.is_none() {
                traffic.quota_remaining = Some(bytes);
            }
            return true;
        }
        // "剩余：无限" etc.
        if is_unlimited_text(rest) {
            return true;
        }
    }

    // 已用流量 / 已使用
    if let Some(rest) = strip_label(name, &["已用流量", "已使用流量", "已用", "已使用"])
    {
        if let Some(bytes) = parse_size_to_bytes(rest) {
            if traffic.download.is_none() && traffic.upload.is_none() {
                traffic.download = Some(bytes);
            }
            return true;
        }
    }

    // 总流量 / 套餐流量
    if let Some(rest) = strip_label(name, &["总流量", "套餐流量", "流量总量", "总量"])
    {
        if let Some(bytes) = parse_size_to_bytes(rest) {
            if traffic.total.is_none() {
                traffic.total = Some(bytes);
            }
            return true;
        }
        if is_unlimited_text(rest) {
            return true;
        }
    }

    // 套餐到期 / 到期时间 / 过期时间
    if let Some(rest) = strip_label(
        name,
        &[
            "套餐到期",
            "到期时间",
            "过期时间",
            "到期日",
            "有效期至",
            "有效期",
            "到期",
            "Expire",
            "expire",
        ],
    ) {
        let rest = rest.trim();
        if rest.is_empty() {
            return true;
        }
        if traffic.expire.is_none() {
            if let Some(ts) = parse_expire_timestamp(rest) {
                traffic.expire = Some(ts);
            } else if traffic.expire_text.is_none() {
                traffic.expire_text = Some(rest.to_string());
            }
        } else if traffic.expire_text.is_none() && parse_expire_timestamp(rest).is_none() {
            traffic.expire_text = Some(rest.to_string());
        }
        return true;
    }

    // English-ish
    let lower = name.to_ascii_lowercase();
    if lower.contains("traffic reset")
        || lower.contains("package expired")
        || lower.starts_with("expire")
        || lower.contains("remaining traffic")
        || lower.contains("traffic remaining")
    {
        if let Some(rest) = name.split_once([':', '：']).map(|(_, r)| r.trim()) {
            if let Some(bytes) = parse_size_to_bytes(rest) {
                if traffic.quota_remaining.is_none()
                    && (lower.contains("remaining") || lower.contains("left"))
                {
                    traffic.quota_remaining = Some(bytes);
                }
            }
            if traffic.expire_text.is_none()
                && (lower.contains("expire") || lower.contains("expired"))
            {
                traffic.expire_text = Some(rest.to_string());
            }
        }
        return true;
    }

    // Bare info labels without value (官网 / 更新订阅 / 公告…)
    if is_pure_info_label(name) {
        return true;
    }

    false
}

fn strip_label<'a>(name: &'a str, labels: &[&str]) -> Option<&'a str> {
    for label in labels {
        if let Some(rest) = name.strip_prefix(label) {
            let rest = rest.trim_start_matches(['：', ':', ' ', '\t', '-', '—']);
            return Some(rest);
        }
        // allow "【剩余流量】2.41 TB"
        let wrapped = format!("【{label}】");
        if let Some(rest) = name.strip_prefix(&wrapped) {
            return Some(rest.trim());
        }
        let wrapped2 = format!("[{label}]");
        if let Some(rest) = name.strip_prefix(&wrapped2) {
            return Some(rest.trim());
        }
    }
    None
}

fn is_unlimited_text(s: &str) -> bool {
    let t = s.trim();
    t == "无限" || t == "无限制" || t.eq_ignore_ascii_case("unlimited") || t == "∞"
}

fn is_pure_info_label(name: &str) -> bool {
    let n = name.trim();
    matches!(
        n,
        "官网"
            | "官方网站"
            | "更新"
            | "更新订阅"
            | "公告"
            | "说明"
            | "教程"
            | "测速"
            | "Traffic"
            | "Expire"
    ) || n.starts_with("官网")
        || n.starts_with("http://")
        || n.starts_with("https://")
}

/// Parse `2.41 TB`, `2.41TB`, `1000G`, `512 MB` → bytes (binary 1024).
fn parse_size_to_bytes(s: &str) -> Option<u64> {
    let s = s.trim().replace(',', "");
    if s.is_empty() || is_unlimited_text(&s) {
        return None;
    }
    // number + optional unit
    let bytes = s.as_bytes();
    let mut i = 0;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        i += 1;
    }
    let start = i;
    while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
        i += 1;
    }
    if i == start {
        return None;
    }
    let num: f64 = std::str::from_utf8(&bytes[start..i]).ok()?.parse().ok()?;
    if !num.is_finite() || num < 0.0 {
        return None;
    }
    let unit = s[i..].trim().to_ascii_lowercase().replace(' ', "");
    let mult: f64 = match unit.as_str() {
        "" | "b" | "byte" | "bytes" => 1.0,
        "k" | "kb" | "kib" => 1024.0,
        "m" | "mb" | "mib" => 1024.0_f64.powi(2),
        "g" | "gb" | "gib" => 1024.0_f64.powi(3),
        "t" | "tb" | "tib" => 1024.0_f64.powi(4),
        "p" | "pb" | "pib" => 1024.0_f64.powi(5),
        _ => return None,
    };
    Some((num * mult).round() as u64)
}

fn parse_expire_timestamp(s: &str) -> Option<i64> {
    let s = s.trim();
    // pure unix seconds
    if let Ok(n) = s.parse::<i64>() {
        if n > 1_000_000_000 {
            return Some(n);
        }
    }
    // YYYY-MM-DD / YYYY/MM/DD / YYYY.MM.DD [HH:MM[:SS]]
    let normalized = s.replace(['/', '.'], "-");
    let date_part = normalized.split_whitespace().next().unwrap_or(&normalized);
    let mut parts = date_part.split('-');
    let y: i32 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    if !(1970..=2100).contains(&y) || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    // Approximate UTC midnight via days since epoch (good enough for display).
    let days = days_from_civil(y, m as i32, d as i32)?;
    Some(days * 86400)
}

/// Howard Hinnant civil-from-days inverse (proleptic Gregorian) → days since 1970-01-01.
fn days_from_civil(y: i32, m: i32, d: i32) -> Option<i64> {
    if m < 1 || m > 12 || d < 1 || d > 31 {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if m > 2 { m - 3 } else { m + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + d as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some((era * 146097 + doe as i64) - 719468)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_name_different_credentials_get_distinct_ids() {
        // Two nodes sharing name/server/port/protocol but differing only by
        // password: compute_id now hashes credentials too, so they get
        // distinct ids directly (no more `node-<id[..16]>` tag collision).
        let mk = |password: &str| ProxyNode {
            id: String::new(),
            name: "香港 01".into(),
            protocol: crate::domain::Protocol::Shadowsocks,
            server: "example.com".into(),
            port: 8388,
            tls: None,
            transport: None,
            udp: None,
            config: crate::domain::ProtocolConfig::Shadowsocks {
                method: "aes-128-gcm".into(),
                password: password.into(),
                plugin: None,
                plugin_opts: None,
                shadow_tls: None,
            },
            source: None,
            latency_ms: None,
            latency_at: None,
        };
        let parsed = ParseResult {
            nodes: vec![mk("pass-a"), mk("pass-b")],
            skipped: vec![],
            format: SubscriptionFormat::UriList,
        };
        let outcome = build_outcome(
            "test-sub".into(),
            SubscriptionSource::Text {
                content: String::new(),
            },
            parsed,
            None,
            false,
        );
        assert_eq!(outcome.nodes.len(), 2);
        assert_ne!(outcome.nodes[0].id, outcome.nodes[1].id);
        // Ids must differ on the 16-hex prefix `outbound_tag` renders.
        assert_ne!(
            outcome.nodes[0].id[..16.min(outcome.nodes[0].id.len())],
            outcome.nodes[1].id[..16.min(outcome.nodes[1].id.len())]
        );
    }

    #[test]
    fn node_id_survives_rename_and_resubscribe() {
        // The whole point of hashing on backend identity instead of name/sub
        // id: an airport renaming a node, or the user re-adding the same
        // subscription under a different URL, must not rotate the node's
        // id — otherwise manual selections and rule bindings silently break.
        let mk = |name: &str| ProxyNode {
            id: String::new(),
            name: name.into(),
            protocol: crate::domain::Protocol::Shadowsocks,
            server: "example.com".into(),
            port: 8388,
            tls: None,
            transport: None,
            udp: None,
            config: crate::domain::ProtocolConfig::Shadowsocks {
                method: "aes-128-gcm".into(),
                password: "same-pass".into(),
                plugin: None,
                plugin_opts: None,
                shadow_tls: None,
            },
            source: None,
            latency_ms: None,
            latency_at: None,
        };
        let build = |name: &str, sub_url: &str| {
            build_outcome(
                "airport".into(),
                SubscriptionSource::Url {
                    url: sub_url.into(),
                },
                ParseResult {
                    nodes: vec![mk(name)],
                    skipped: vec![],
                    format: SubscriptionFormat::UriList,
                },
                None,
                false,
            )
        };
        // Same node, renamed by the airport on refresh.
        let before = build("HK-01", "https://sub.example.com/a");
        let after_rename = build("HK-01-renamed", "https://sub.example.com/a");
        assert_eq!(before.nodes[0].id, after_rename.nodes[0].id);

        // Same node, subscription re-added under a different URL.
        let after_resub = build("HK-01", "https://sub.example.com/b");
        assert_eq!(before.nodes[0].id, after_resub.nodes[0].id);
    }

    #[test]
    fn parse_userinfo_basic() {
        let t = parse_userinfo_str(
            "upload=1073741824; download=2147483648; total=1073741824000; expire=1893456000",
        )
        .expect("parsed");
        assert_eq!(t.upload, Some(1_073_741_824));
        assert_eq!(t.download, Some(2_147_483_648));
        assert_eq!(t.total, Some(1_073_741_824_000));
        assert_eq!(t.expire, Some(1_893_456_000));
        assert_eq!(t.used(), 3_221_225_472);
        assert!(t.used_ratio().unwrap() < 0.01);
        assert!(t.remaining().unwrap() > 1_000_000_000_000);
    }

    #[test]
    fn parse_userinfo_like_flclash() {
        // Same string shape FlClash tests use
        let t = parse_userinfo_str("upload=10; download=20; total=100; expire=200").unwrap();
        assert_eq!(t.upload, Some(10));
        assert_eq!(t.download, Some(20));
        assert_eq!(t.total, Some(100));
        assert_eq!(t.used(), 30);
        assert!((t.used_ratio().unwrap() - 0.3).abs() < 1e-9);
    }

    #[test]
    fn subscription_ua_contains_clash_verge() {
        let ua = subscription_user_agent();
        assert!(ua.to_ascii_lowercase().contains("clash-verge"), "ua={ua}");
        assert!(ua.to_ascii_lowercase().contains("flclash"), "ua={ua}");
    }

    #[test]
    fn parse_disposition_filename_star_utf8() {
        // 良心云
        let d = "attachment;filename*=UTF-8''%E8%89%AF%E5%BF%83%E4%BA%91";
        let name = parse_disposition_filename_str(d).expect("name");
        assert_eq!(name, "良心云");
    }

    #[test]
    fn parse_disposition_filename_star_with_ext() {
        let d = r#"attachment; filename*=UTF-8''%E6%B5%8B%E8%AF%95.yaml"#;
        let name = parse_disposition_filename_str(d).expect("name");
        assert_eq!(name, "测试");
    }

    #[test]
    fn parse_disposition_filename_plain() {
        let d = r#"attachment; filename="my-sub.yaml""#;
        let name = parse_disposition_filename_str(d).expect("name");
        assert_eq!(name, "my-sub");
    }

    #[test]
    fn parse_disposition_prefers_star_over_plain() {
        let d = r#"attachment; filename="fallback.yaml"; filename*=UTF-8''%E4%BC%98%E5%85%88"#;
        let name = parse_disposition_filename_str(d).expect("name");
        assert_eq!(name, "优先");
    }

    #[test]
    fn parse_userinfo_empty() {
        assert!(parse_userinfo_str("").is_none());
        assert!(parse_userinfo_str("foo=bar").is_none());
    }

    #[test]
    fn parse_userinfo_from_yaml_comment() {
        let yaml = r#"# upload=455727941; download=6174315083; total=1073741824000; expire=1671815872;

proxies:
  - name: a
    type: ss
"#;
        let t = parse_userinfo_from_content(yaml).expect("body comment");
        assert_eq!(t.upload, Some(455_727_941));
        assert_eq!(t.download, Some(6_174_315_083));
        assert_eq!(t.total, Some(1_073_741_824_000));
        assert_eq!(t.expire, Some(1_671_815_872));
    }

    #[test]
    fn parse_size_tb() {
        let b = parse_size_to_bytes("2.41 TB").unwrap();
        assert!((b as f64 - 2.41 * 1024f64.powi(4)).abs() < 1024.0 * 1024.0);
        assert_eq!(parse_size_to_bytes("1000G").unwrap(), 1000 * 1024u64.pow(3));
    }

    #[test]
    fn remark_remaining_and_expire() {
        let mut t = SubscriptionTraffic::default();
        assert!(apply_remark_name("剩余流量：2.41 TB", &mut t));
        assert!(apply_remark_name("套餐到期：长期有效", &mut t));
        assert_eq!(t.expire_text.as_deref(), Some("长期有效"));
        let rem = t.quota_remaining.unwrap();
        assert!(rem > 2 * 1024u64.pow(4));
        assert!(rem < 3 * 1024u64.pow(4));
    }

    #[test]
    fn split_filters_remark_nodes() {
        use crate::domain::{Protocol, ProtocolConfig, ProxyNode};
        let mk = |name: &str| ProxyNode {
            id: name.into(),
            name: name.into(),
            protocol: Protocol::Vless,
            server: "cfyes.example.com".into(),
            port: 443,
            tls: None,
            transport: None,
            udp: None,
            config: ProtocolConfig::Vless {
                uuid: "x".into(),
                flow: None,
                packet_encoding: "xudp".into(),
            },
            source: None,
            latency_ms: None,
            latency_at: None,
        };
        let (traffic, real) = split_remark_nodes(vec![
            mk("剩余流量：2.41 TB"),
            mk("套餐到期：长期有效"),
            mk("HK-01"),
        ]);
        assert_eq!(real.len(), 1);
        assert_eq!(real[0].name, "HK-01");
        let t = traffic.unwrap();
        assert!(t.quota_remaining.is_some());
        assert_eq!(t.expire_text.as_deref(), Some("长期有效"));
    }

    #[test]
    fn local_link_parse_keeps_hysteria2_named_like_quota() {
        let uri = "hysteria2://8df42c5a-e1c4-44b8-8806-463573d05ac1@203.10.98.188:443/?insecure=false&sni=www.bing.com#%E5%89%A9%E4%BD%99%E6%B5%81%E9%87%8F%EF%BC%9A977.82%20GB";
        let outcome = import_from_text(Some("hy2-test".into()), uri.into(), None).unwrap();
        assert_eq!(outcome.nodes.len(), 1);
        assert_eq!(outcome.subscription.node_count, 1);
        assert!(outcome.subscription.traffic.is_none());
        assert_eq!(outcome.nodes[0].server, "203.10.98.188");
        assert_eq!(outcome.nodes[0].port, 443);
        assert_ne!(outcome.nodes[0].name, "剩余流量：977.82 GB");
    }

    #[test]
    fn same_hysteria2_with_different_fragments_is_one_node() {
        let a = "hysteria2://8df42c5a-e1c4-44b8-8806-463573d05ac1@203.10.98.188:443/?insecure=false&sni=www.bing.com#%E5%89%A9%E4%BD%99%E6%B5%81%E9%87%8F%EF%BC%9A977.82%20GB";
        let b = "hysteria2://8df42c5a-e1c4-44b8-8806-463573d05ac1@203.10.98.188:443/?insecure=false&sni=www.bing.com#%E5%A5%97%E9%A4%90%E5%88%B0%E6%9C%9F%EF%BC%9A%E9%95%BF%E6%9C%9F%E6%9C%89%E6%95%88";
        let outcome = import_from_text(Some("hy2-dup".into()), format!("{a}\n{b}"), None).unwrap();
        assert_eq!(outcome.nodes.len(), 1);
        assert_eq!(outcome.subscription.node_count, 1);
    }

    #[test]
    fn local_import_of_full_clash_config_matches_parser() {
        let yaml = include_str!("../../tests/fixtures/clash_free_sample.yaml");
        let outcome = import_from_text(Some("clash".into()), yaml.into(), None).unwrap();
        assert_eq!(outcome.nodes.len(), 8);
        assert_eq!(outcome.subscription.node_count, 8);
        assert_eq!(outcome.subscription.skipped_count, 0);
        assert_eq!(outcome.subscription.format.as_deref(), Some("clash_yaml"));
    }

    #[test]
    fn same_backend_different_names_stay_separate() {
        let base = "hysteria2://8df42c5a-e1c4-44b8-8806-463573d05ac1@203.10.98.188:443/?insecure=false&sni=www.bing.com";
        let body = format!("{base}#HK-01\n{base}#HK-02\n{base}#JP-01");
        let outcome = import_from_text(Some("airport".into()), body, None).unwrap();
        assert_eq!(outcome.nodes.len(), 3);
        assert_eq!(outcome.subscription.node_count, 3);
        let mut names: Vec<_> = outcome.nodes.iter().map(|n| n.name.as_str()).collect();
        names.sort_unstable();
        assert_eq!(names, ["HK-01", "HK-02", "JP-01"]);
    }
}

pub fn import_from_file(name: Option<String>, path: &Path) -> AppResult<ImportOutcome> {
    import_from_file_with_id(name, path, None)
}

pub fn import_from_file_with_id(
    name: Option<String>,
    path: &Path,
    existing_id: Option<String>,
) -> AppResult<ImportOutcome> {
    if !path.exists() {
        return Err(AppError::Io(format!("file not found: {}", path.display())));
    }
    let meta = std::fs::metadata(path)?;
    if meta.len() as usize > MAX_BODY_BYTES {
        return Err(AppError::Io(format!(
            "file too large ({} bytes, max {})",
            meta.len(),
            MAX_BODY_BYTES
        )));
    }
    let content = std::fs::read_to_string(path)?;
    let display_name = name
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| {
            path.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("Local config")
                .to_string()
        });
    // Copy file bytes into the store — do not keep a live path (unstable).
    import_from_text(Some(display_name), content, existing_id)
}

pub fn import_from_text(
    name: Option<String>,
    content: String,
    existing_id: Option<String>,
) -> AppResult<ImportOutcome> {
    let content = content.trim().to_string();
    if content.is_empty() {
        return Err(AppError::EmptySubscription);
    }
    if content.len() > MAX_BODY_BYTES {
        return Err(AppError::Io(format!(
            "config too large ({} bytes, max {})",
            content.len(),
            MAX_BODY_BYTES
        )));
    }
    let display_name = name
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "多节点".into());
    let mut outcome = import_parsed_content(
        display_name,
        &content,
        SubscriptionSource::Text {
            content: content.clone(),
        },
        existing_id,
    )?;
    outcome.subscription.auto_update = false;
    Ok(outcome)
}

pub fn import_from_singbox(
    name: Option<String>,
    content: String,
    existing_id: Option<String>,
) -> AppResult<ImportOutcome> {
    let normalized = validate_complete_singbox_config(&content)?;
    let display_name = name
        .filter(|s| !s.trim().is_empty())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "sing-box".into());
    let source = SubscriptionSource::Singbox {
        content: normalized,
    };
    let parsed = crate::domain::ParseResult {
        nodes: Vec::new(),
        skipped: Vec::new(),
        format: crate::domain::SubscriptionFormat::SingboxJson,
    };
    let mut outcome = build_outcome(display_name, source, parsed, existing_id, false);
    outcome.subscription.auto_update = false;
    outcome.subscription.enabled = false;
    Ok(outcome)
}

pub fn import_from_node(
    name: Option<String>,
    uri: Option<String>,
    draft: Option<ManualNodeDraft>,
    existing_id: Option<String>,
) -> AppResult<ImportOutcome> {
    let display_name = name
        .as_ref()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
    let uri = uri.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

    let (parsed, source) = if let Some(uri) = uri {
        let parsed = parse_single_uri(&uri, display_name.as_deref())?;
        (parsed, SubscriptionSource::Node { uri: Some(uri) })
    } else if let Some(draft) = draft {
        let parsed = parse_manual_draft(&draft, display_name.as_deref())?;
        (parsed, SubscriptionSource::Node { uri: None })
    } else {
        return Err(AppError::SubscriptionParse(
            "node requires a share URI or a filled form".into(),
        ));
    };

    let name = display_name.unwrap_or_else(|| {
        parsed
            .nodes
            .first()
            .map(|n| n.name.clone())
            .unwrap_or_else(|| "Node".into())
    });
    let mut outcome = build_outcome(name, source, parsed, existing_id, false);
    outcome.subscription.auto_update = false;
    Ok(outcome)
}

fn import_parsed_content(
    display_name: String,
    content: &str,
    source: SubscriptionSource,
    existing_id: Option<String>,
) -> AppResult<ImportOutcome> {
    let parsed = parse_subscription(content)?;
    let extract_quota = source.is_remote();
    let body_traffic = if extract_quota {
        parse_userinfo_from_content(content)
    } else {
        None
    };
    let mut outcome = build_outcome(display_name, source, parsed, existing_id, extract_quota);
    if extract_quota {
        outcome.subscription.traffic =
            SubscriptionTraffic::merge(body_traffic, outcome.subscription.traffic);
    }
    Ok(outcome)
}

fn build_outcome(
    name: String,
    source: SubscriptionSource,
    parsed: ParseResult,
    existing_id: Option<String>,
    extract_quota: bool,
) -> ImportOutcome {
    let id = existing_id.unwrap_or_else(|| subscription_id(&source));
    let format = format_label(parsed.format);
    let skipped = parsed.skipped.len();
    let (remark_traffic, real_nodes) = if extract_quota {
        split_remark_nodes(parsed.nodes)
    } else {
        let nodes: Vec<ProxyNode> = parsed
            .nodes
            .into_iter()
            .map(|mut n| {
                if is_remark_name(&n.name) {
                    n.name = format!("{}-{}-{}", n.protocol.as_str(), n.server, n.port);
                }
                n
            })
            .collect();
        (None, nodes)
    };
    let real_nodes = dedupe_nodes(real_nodes);
    let node_count = real_nodes.len() as u32;
    let subscription = Subscription {
        id,
        name,
        source,
        last_update: now_secs(),
        node_count,
        enabled: true,
        format: Some(format),
        skipped_count: skipped as u32,
        via_proxy: false,
        auto_update: false,
        auto_update_interval_min: 1440,
        traffic: remark_traffic,
        user_agent: None,
    };

    // Re-hash node ids on backend identity (server/port/protocol/credentials)
    // so a subscription refresh that only renames the airport/node, or
    // rotates the subscription URL, doesn't rotate the node's id — manual
    // selections and rule bindings survive across refreshes as long as the
    // underlying host/port/auth stay the same.
    let mut nodes: Vec<ProxyNode> = real_nodes
        .into_iter()
        .map(|mut n| {
            n = n.with_computed_id();
            // latency filled later by probe; clear on fresh parse
            n.latency_ms = None;
            n.latency_at = None;
            n
        })
        .collect();
    // Same-server/port/protocol nodes with identical credentials but
    // different remarks (an airport listing one backend under two names)
    // would collide on the outbound tag (`node-<id[..16]>`) and make
    // `sing-box check` fail with `duplicate outbound/endpoint tag`.
    let renamed = ProxyNode::ensure_unique_ids(nodes.iter_mut());
    if renamed > 0 {
        crate::app_log::warn(
            "import",
            format!("{renamed} 个节点与同订阅节点标识冲突，已改写 id 以避免 tag 重复"),
        );
    }
    ImportOutcome {
        subscription,
        nodes,
        skipped: parsed.skipped,
    }
}

fn dedupe_nodes(nodes: Vec<ProxyNode>) -> Vec<ProxyNode> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(nodes.len());
    for node in nodes {
        // Keep HK-01 / HK-02 even when they share host:port:auth.
        // Remark fragments (剩余流量 / 套餐到期) are rewritten to the same
        // proto-host-port name first, so those still collapse to one node.
        if seen.insert(node.instance_key()) {
            out.push(node);
        }
    }
    out
}

fn subscription_id(source: &SubscriptionSource) -> String {
    let mut hasher = Sha256::new();
    match source {
        SubscriptionSource::Url { url } => {
            hasher.update(b"url|");
            let canonical =
                canonical_subscription_url(url).unwrap_or_else(|| url.trim().to_string());
            hasher.update(canonical.as_bytes());
        }
        SubscriptionSource::File { path } => {
            hasher.update(b"file|");
            hasher.update(path.as_bytes());
        }
        SubscriptionSource::Text { content } => {
            hasher.update(b"text|");
            hasher.update(content.as_bytes());
        }
        SubscriptionSource::Node { uri } => {
            hasher.update(b"node|");
            if let Some(uri) = uri {
                hasher.update(uri.as_bytes());
            } else {
                hasher.update(b"manual");
                hasher.update(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_nanos().to_string())
                        .unwrap_or_default()
                        .as_bytes(),
                );
            }
        }
        SubscriptionSource::Singbox { content } => {
            hasher.update(b"singbox|");
            hasher.update(content.as_bytes());
        }
    }
    let digest = hasher.finalize();
    hex::encode(&digest[..16])
}

#[cfg(test)]
mod canonical_url_tests {
    use super::canonical_subscription_url;

    #[test]
    fn normalizes_only_safe_url_parts() {
        assert_eq!(
            canonical_subscription_url(" HTTPS://Example.COM:443#view "),
            Some("https://example.com/".into())
        );
        assert_eq!(
            canonical_subscription_url("http://example.com:80/path?b=2&a=1"),
            Some("http://example.com/path?b=2&a=1".into())
        );
    }

    #[test]
    fn preserves_sensitive_path_and_query_order() {
        assert_ne!(
            canonical_subscription_url("https://example.com/Token?a=1&b=2"),
            canonical_subscription_url("https://example.com/token?b=2&a=1")
        );
    }
}

fn name_from_url(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
        .unwrap_or_else(|| "Subscription".into())
}

fn format_label(f: SubscriptionFormat) -> String {
    match f {
        SubscriptionFormat::ClashYaml => "clash_yaml".into(),
        SubscriptionFormat::UriList => "uri_list".into(),
        SubscriptionFormat::Base64UriList => "base64_uri_list".into(),
        SubscriptionFormat::SingboxJson => "singbox_json".into(),
        SubscriptionFormat::Manual => "manual".into(),
    }
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
