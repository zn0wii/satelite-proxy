use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// How a subscription was imported.
///
/// Serialized so older builds (url/file only) still load the store: `text` /
/// `node` are written as `kind=file` plus a `profile` marker. New builds read
/// both the compatible form and the explicit `text`/`node` tags.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubscriptionSource {
    Url {
        url: String,
    },
    File {
        path: String,
    },
    /// Pasted config body (sing-box / Clash / URI list).
    Text {
        content: String,
    },
    /// Single node: share URI and/or a form-built node.
    Node {
        uri: Option<String>,
    },
    /// Complete sing-box JSON, launched as-is (not generated).
    Singbox {
        content: String,
    },
}

const NODE_SENTINEL: &str = "satelite:node";
const TEXT_SENTINEL: &str = "satelite:text";
const SINGBOX_SENTINEL: &str = "satelite:singbox";

impl SubscriptionSource {
    /// Whether this profile feeds nodes into the generated sing-box config.
    pub fn contributes_nodes(&self) -> bool {
        !matches!(self, Self::Singbox { .. })
    }

    pub fn is_remote(&self) -> bool {
        matches!(self, Self::Url { .. })
    }
}

#[derive(Serialize, Deserialize)]
struct SourceWire {
    kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    uri: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile: Option<String>,
}

impl Serialize for SubscriptionSource {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let wire = match self {
            Self::Url { url } => SourceWire {
                kind: "url".into(),
                url: Some(url.clone()),
                path: None,
                content: None,
                uri: None,
                profile: None,
            },
            Self::File { path } => SourceWire {
                kind: "file".into(),
                url: None,
                path: Some(path.clone()),
                content: None,
                uri: None,
                profile: None,
            },
            Self::Text { content } => SourceWire {
                kind: "file".into(),
                url: None,
                path: Some(TEXT_SENTINEL.into()),
                content: Some(content.clone()),
                uri: None,
                profile: Some("text".into()),
            },
            Self::Node { uri } => SourceWire {
                kind: "file".into(),
                url: None,
                path: Some(NODE_SENTINEL.into()),
                content: None,
                uri: uri.clone(),
                profile: Some("node".into()),
            },
            Self::Singbox { content } => SourceWire {
                kind: "file".into(),
                url: None,
                path: Some(SINGBOX_SENTINEL.into()),
                content: Some(content.clone()),
                uri: None,
                profile: Some("singbox".into()),
            },
        };
        wire.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SubscriptionSource {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let wire = SourceWire::deserialize(deserializer)?;
        source_from_wire(wire).map_err(serde::de::Error::custom)
    }
}

fn source_from_wire(wire: SourceWire) -> Result<SubscriptionSource, String> {
    let kind = wire.kind.trim().to_ascii_lowercase();
    let profile = wire
        .profile
        .as_deref()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let path = wire.path.as_deref().unwrap_or("").trim();
    if kind == "url" {
        return Ok(SubscriptionSource::Url {
            url: wire.url.unwrap_or_default(),
        });
    }
    if kind == "text" || profile == "text" || path == TEXT_SENTINEL {
        return Ok(SubscriptionSource::Text {
            content: wire.content.unwrap_or_default(),
        });
    }
    if kind == "node" || profile == "node" || path == NODE_SENTINEL {
        return Ok(SubscriptionSource::Node {
            uri: wire.uri.filter(|s| !s.trim().is_empty()),
        });
    }
    if kind == "singbox" || profile == "singbox" || path == SINGBOX_SENTINEL {
        return Ok(SubscriptionSource::Singbox {
            content: wire.content.unwrap_or_default(),
        });
    }
    if kind == "file" || kind.is_empty() {
        return Ok(SubscriptionSource::File {
            path: path.to_string(),
        });
    }
    Err(format!("unknown subscription source kind: {kind}"))
}

/// Traffic quota from `subscription-userinfo` header and/or remark node names.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SubscriptionTraffic {
    /// Upload used (bytes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload: Option<u64>,
    /// Download used (bytes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub download: Option<u64>,
    /// Total quota (bytes). 0 or missing means unlimited / unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
    /// Explicit remaining (bytes) from remark nodes like `剩余流量：2.41 TB`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quota_remaining: Option<u64>,
    /// Expire time as Unix timestamp (seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expire: Option<i64>,
    /// Human-readable expire when not a timestamp (e.g. `长期有效`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expire_text: Option<String>,
}

impl SubscriptionTraffic {
    pub fn used(&self) -> u64 {
        self.upload
            .unwrap_or(0)
            .saturating_add(self.download.unwrap_or(0))
    }

    pub fn remaining(&self) -> Option<u64> {
        if let Some(r) = self.quota_remaining {
            return Some(r);
        }
        let total = self.total.filter(|&t| t > 0)?;
        Some(total.saturating_sub(self.used()))
    }

    /// Used ratio 0.0–1.0 when total is known and > 0.
    pub fn used_ratio(&self) -> Option<f64> {
        let total = self.total.filter(|&t| t > 0)? as f64;
        if let Some(rem) = self.quota_remaining {
            let used = (total - rem as f64).max(0.0);
            return Some((used / total).clamp(0.0, 1.0));
        }
        Some((self.used() as f64 / total).clamp(0.0, 1.0))
    }

    pub fn is_empty(&self) -> bool {
        self.upload.is_none()
            && self.download.is_none()
            && self.total.is_none()
            && self.quota_remaining.is_none()
            && self.expire.is_none()
            && self.expire_text.is_none()
    }

    /// Prefer non-empty fields from `primary`, fill gaps from `fallback`.
    pub fn merge(primary: Option<Self>, fallback: Option<Self>) -> Option<Self> {
        match (primary, fallback) {
            (None, None) => None,
            (Some(a), None) | (None, Some(a)) => {
                if a.is_empty() {
                    None
                } else {
                    Some(a)
                }
            }
            (Some(mut a), Some(b)) => {
                if a.upload.is_none() {
                    a.upload = b.upload;
                }
                if a.download.is_none() {
                    a.download = b.download;
                }
                if a.total.is_none() {
                    a.total = b.total;
                }
                if a.quota_remaining.is_none() {
                    a.quota_remaining = b.quota_remaining;
                }
                if a.expire.is_none() {
                    a.expire = b.expire;
                }
                if a.expire_text.is_none() {
                    a.expire_text = b.expire_text;
                }
                if a.is_empty() {
                    None
                } else {
                    Some(a)
                }
            }
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub id: String,
    pub name: String,
    pub source: SubscriptionSource,
    /// Unix timestamp (seconds).
    pub last_update: i64,
    pub node_count: u32,
    pub enabled: bool,
    /// Detected format label, e.g. clash_yaml / uri_list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Nodes skipped on last import.
    #[serde(default)]
    pub skipped_count: u32,
    /// Fetch subscription URL via local mixed proxy (127.0.0.1:mixed_port).
    #[serde(default)]
    pub via_proxy: bool,
    /// Periodically re-fetch / re-read this profile.
    #[serde(default)]
    pub auto_update: bool,
    /// Auto-update interval in minutes (default 1440 = 24h). Minimum 1.
    #[serde(default = "default_auto_update_interval_min")]
    pub auto_update_interval_min: u32,
    /// Traffic / expire from last URL fetch (`subscription-userinfo`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traffic: Option<SubscriptionTraffic>,
    /// Custom User-Agent for URL fetches. Empty/absent falls back to the
    /// built-in Clash-like default (see `subscription_user_agent`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
}

fn default_auto_update_interval_min() -> u32 {
    1440
}

/// Summary returned to UI (URL masked in list).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionView {
    pub id: String,
    pub name: String,
    pub source_kind: String,
    /// Display-only source (URL may be redacted; file = basename).
    pub source_display: String,
    pub last_update: i64,
    pub node_count: u32,
    pub enabled: bool,
    pub format: Option<String>,
    pub skipped_count: u32,
    #[serde(default)]
    pub auto_update: bool,
    #[serde(default = "default_auto_update_interval_min")]
    pub auto_update_interval_min: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traffic: Option<SubscriptionTraffic>,
    /// First (or only) node facts — filled for single-node profiles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_summary: Option<super::NodeSummary>,
}

/// Full fields for edit form (includes raw URL / path).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionDetail {
    pub id: String,
    pub name: String,
    pub source_kind: String,
    pub url: Option<String>,
    pub path: Option<String>,
    /// Pasted config body (text profiles).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Share URI for a single-node profile.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uri: Option<String>,
    /// Flattened node form (node profiles, for edit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<super::ManualNodeDraft>,
    pub last_update: i64,
    pub node_count: u32,
    pub enabled: bool,
    pub format: Option<String>,
    pub skipped_count: u32,
    pub via_proxy: bool,
    #[serde(default)]
    pub auto_update: bool,
    #[serde(default = "default_auto_update_interval_min")]
    pub auto_update_interval_min: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub traffic: Option<SubscriptionTraffic>,
    /// Custom User-Agent for URL fetches (empty = built-in default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
}

impl Subscription {
    pub fn to_view(&self) -> SubscriptionView {
        let (source_kind, source_display) = match &self.source {
            SubscriptionSource::Url { url } => ("url".into(), mask_url_for_display(url)),
            SubscriptionSource::File { path } => {
                let name = std::path::Path::new(path)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or(path)
                    .to_string();
                ("file".into(), name)
            }
            SubscriptionSource::Text { content } => {
                let label = first_nonempty_line(content).unwrap_or_else(|| "pasted config".into());
                ("text".into(), truncate_display(&label, 48))
            }
            SubscriptionSource::Node { uri } => {
                let display = uri
                    .as_deref()
                    .map(mask_share_uri)
                    .unwrap_or_else(|| "manual node".into());
                ("node".into(), display)
            }
            SubscriptionSource::Singbox { .. } => ("singbox".into(), "sing-box".into()),
        };
        SubscriptionView {
            id: self.id.clone(),
            name: self.name.clone(),
            source_kind,
            source_display,
            last_update: self.last_update,
            node_count: self.node_count,
            enabled: self.enabled,
            format: self.format.clone(),
            skipped_count: self.skipped_count,
            auto_update: self.auto_update,
            auto_update_interval_min: self.auto_update_interval_min.max(1),
            traffic: self.traffic.clone(),
            node_summary: None,
        }
    }

    pub fn to_detail(&self) -> SubscriptionDetail {
        let base = |source_kind: &str| SubscriptionDetail {
            id: self.id.clone(),
            name: self.name.clone(),
            source_kind: source_kind.into(),
            url: None,
            path: None,
            content: None,
            uri: None,
            node: None,
            last_update: self.last_update,
            node_count: self.node_count,
            enabled: self.enabled,
            format: self.format.clone(),
            skipped_count: self.skipped_count,
            via_proxy: self.via_proxy,
            auto_update: self.auto_update,
            auto_update_interval_min: self.auto_update_interval_min.max(1),
            traffic: self.traffic.clone(),
            user_agent: self.user_agent.clone(),
        };
        match &self.source {
            SubscriptionSource::Url { url } => SubscriptionDetail {
                url: Some(url.clone()),
                ..base("url")
            },
            SubscriptionSource::File { path } => SubscriptionDetail {
                path: Some(path.clone()),
                ..base("file")
            },
            SubscriptionSource::Text { content } => SubscriptionDetail {
                content: Some(content.clone()),
                ..base("text")
            },
            SubscriptionSource::Node { uri } => SubscriptionDetail {
                uri: uri.clone(),
                ..base("node")
            },
            SubscriptionSource::Singbox { content } => SubscriptionDetail {
                content: Some(content.clone()),
                ..base("singbox")
            },
        }
    }

    pub fn is_auto_update_due(&self, now_secs: i64) -> bool {
        if !self.auto_update {
            return false;
        }
        let interval = (self.auto_update_interval_min.max(1) as i64).saturating_mul(60);
        now_secs.saturating_sub(self.last_update) >= interval
    }
}

fn first_nonempty_line(s: &str) -> Option<String> {
    s.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(ToString::to_string)
}

fn truncate_display(s: &str, max: usize) -> String {
    let mut it = s.chars();
    let head: String = it.by_ref().take(max).collect();
    if it.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn mask_share_uri(uri: &str) -> String {
    let uri = uri.trim();
    if let Some((scheme, rest)) = uri.split_once("://") {
        if let Some((host_part, _)) = rest.split_once(['?', '#', '/']) {
            let host = host_part.rsplit('@').next().unwrap_or(host_part);
            return format!("{scheme}://{host}");
        }
        if let Some(host) = rest.rsplit('@').next() {
            return format!("{scheme}://{host}");
        }
    }
    truncate_display(uri, 40)
}

/// Hide query string / token-looking tails for UI lists (not full secret storage).
fn mask_url_for_display(url: &str) -> String {
    if let Ok(parsed) = url::Url::parse(url) {
        let host = parsed.host_str().unwrap_or("?");
        let path = parsed.path();
        let short_path = if path.len() > 24 {
            format!("{}…", &path[..24])
        } else {
            path.to_string()
        };
        if parsed.query().is_some() {
            return format!("{}://{}{}?…", parsed.scheme(), host, short_path);
        }
        return format!("{}://{}{}", parsed.scheme(), host, short_path);
    }
    if url.len() > 48 {
        format!("{}…", &url[..48])
    } else {
        url.to_string()
    }
}

#[cfg(test)]
mod source_serde_tests {
    use super::*;

    #[test]
    fn node_roundtrip_is_readable_by_url_file_only_builds() {
        let src = SubscriptionSource::Node {
            uri: Some("vless://uuid@host.example:443#n".into()),
        };
        let json = serde_json::to_value(&src).unwrap();
        assert_eq!(json["kind"], "file");
        assert_eq!(json["profile"], "node");
        assert_eq!(json["path"], "satelite:node");
        let back: SubscriptionSource = serde_json::from_value(json).unwrap();
        assert_eq!(src, back);
    }

    #[test]
    fn reads_explicit_node_tag_from_newer_files() {
        let src: SubscriptionSource =
            serde_json::from_str(r#"{"kind":"node","uri":"trojan://pwd@host.example:443"}"#)
                .unwrap();
        assert_eq!(
            src,
            SubscriptionSource::Node {
                uri: Some("trojan://pwd@host.example:443".into()),
            }
        );
    }

    #[test]
    fn text_roundtrip_uses_compatible_file_tag() {
        let src = SubscriptionSource::Text {
            content: "{\"outbounds\":[]}".into(),
        };
        let json = serde_json::to_value(&src).unwrap();
        assert_eq!(json["kind"], "file");
        assert_eq!(json["profile"], "text");
        let back: SubscriptionSource = serde_json::from_value(json).unwrap();
        assert_eq!(src, back);
    }

    #[test]
    fn singbox_roundtrip_uses_compatible_file_tag() {
        let src = SubscriptionSource::Singbox {
            content: "{\"inbounds\":[],\"outbounds\":[]}".into(),
        };
        let json = serde_json::to_value(&src).unwrap();
        assert_eq!(json["kind"], "file");
        assert_eq!(json["profile"], "singbox");
        let back: SubscriptionSource = serde_json::from_value(json).unwrap();
        assert_eq!(src, back);
    }

    #[test]
    fn unknown_source_kind_is_an_error_not_a_file() {
        let err = serde_json::from_str::<SubscriptionSource>(r#"{"kind":"quantum"}"#).unwrap_err();
        assert!(err.to_string().contains("unknown subscription source kind"));
    }
}
