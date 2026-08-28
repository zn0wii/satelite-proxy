export type NavKey =
  | "dashboard"
  | "config"
  | "nodes"
  | "traffic"
  | "logs"
  | "settings";

export type DnsFinalStrategy = "local" | "domestic" | "remote";
export type DomainMatcher = "domain" | "domain_suffix" | "domain_keyword";

export type DnsAction =
  | { kind: "local" }
  | { kind: "domestic" }
  | { kind: "remote" }
  | { kind: "block" };

export interface DnsRule {
  id: string;
  enabled: boolean;
  matcher: DomainMatcher;
  payload: string;
  action: DnsAction;
}

export interface FakeIpConfig {
  enabled: boolean;
  inet4_range: string;
  inet6_enabled: boolean;
  inet6_range: string;
  bypass: string[];
}

export interface HostsEntry {
  id: string;
  enabled: boolean;
  domain: string;
  addr: string;
}

export interface HostsConfig {
  enabled: boolean;
  include_system: boolean;
  entries: HostsEntry[];
}

export type DnsRuleSetKind = "dns" | "hosts";

export interface DnsRuleSet {
  id: string;
  name: string;
  kind: DnsRuleSetKind;
  builtin: boolean;
  read_only: boolean;
  enabled: boolean;
  dns_rules: DnsRule[];
  hosts: HostsEntry[];
}

export interface DnsSettings {
  enabled: boolean;
  rules_enabled: boolean;
  rules: DnsRule[];
  fake_ip: FakeIpConfig;
  hosts: HostsConfig;
  rule_sets: DnsRuleSet[];
  unified_rules: boolean;
  hijack: boolean;
  cache: boolean;
  leak_protect: boolean;
  /** Default resolver for domains unmatched by a rule set. */
  dns_final: DnsFinalStrategy;
}

export interface DnsTestResult {
  domain: string;
  ok: boolean;
  addrs: string[];
  elapsed_ms: number;
  error?: string | null;
  note: string;
}

/** From subscription-userinfo header and/or remark node names. */
export interface SubscriptionTraffic {
  upload?: number | null;
  download?: number | null;
  total?: number | null;
  /** Explicit remaining bytes (e.g. from `剩余流量：2.41 TB`). */
  quota_remaining?: number | null;
  expire?: number | null;
  /** Human-readable expire when not a unix timestamp (e.g. `长期有效`). */
  expire_text?: string | null;
}

export interface SubscriptionView {
  id: string;
  name: string;
  source_kind: "url" | "file" | "text" | "node" | "singbox" | string;
  source_display: string;
  last_update: number;
  node_count: number;
  enabled: boolean;
  format?: string | null;
  skipped_count: number;
  /** Periodically refresh this profile. */
  auto_update?: boolean;
  /** Minutes between auto updates (default 1440). */
  auto_update_interval_min?: number;
  traffic?: SubscriptionTraffic | null;
}

/** Full subscription for edit form (raw url/path). */
export interface SubscriptionDetail {
  id: string;
  name: string;
  source_kind: "url" | "file" | "text" | "node" | "singbox" | string;
  url?: string | null;
  path?: string | null;
  content?: string | null;
  uri?: string | null;
  node?: ManualNodeDraft | null;
  last_update: number;
  node_count: number;
  enabled: boolean;
  format?: string | null;
  skipped_count: number;
  via_proxy: boolean;
  auto_update?: boolean;
  auto_update_interval_min?: number;
  traffic?: SubscriptionTraffic | null;
}

export interface SubscriptionUrlEntry {
  id: string;
  url: string;
}

export interface ImportResult {
  subscription: SubscriptionView;
  node_count: number;
  skipped_count: number;
}

export interface ProxyNode {
  id: string;
  name: string;
  protocol: string;
  server: string;
  port: number;
  source?: string;
  latency_ms?: number | null;
  latency_at?: number | null;
  /** Present from list_all_nodes — owning subscription. */
  subscription_id?: string;
  subscription_name?: string;
}

export type ViewMode = "list" | "grid";
export type SortMode = "default" | "name" | "latency";

export interface NodePage {
  nodes: ProxyNode[];
  total: number;
  offset: number;
}

export interface LatencyResult {
  id: string;
  name: string;
  latency_ms?: number | null;
  error?: string | null;
  tested_at: number;
  /** `tcp` | `clash_api` | `unsupported` (needs core running) */
  method?: string;
}

export interface LatencyBatchResult {
  results: LatencyResult[];
  tested: number;
  ok: number;
  failed: number;
  method?: string;
}

export type AddSourceKind = "url" | "file" | "text" | "node" | "singbox";

export type ProfileKind = "subscription" | "local" | "singbox";
export type LocalKind = "node" | "multi";
export type ConfigInputMode = "paste" | "file";

/** Flattened single-node form. Field names match the Rust `ManualNodeDraft`. */
export interface ManualNodeDraft {
  protocol: string;
  server: string;
  port: number;
  name?: string | null;
  password?: string | null;
  uuid?: string | null;
  method?: string | null;
  plugin?: string | null;
  pluginOpts?: string | null;
  alterId?: number | null;
  security?: string | null;
  flow?: string | null;
  packetEncoding?: string | null;
  username?: string | null;
  path?: string | null;
  upMbps?: number | null;
  downMbps?: number | null;
  obfs?: string | null;
  obfsPassword?: string | null;
  congestionControl?: string | null;
  udpRelayMode?: string | null;
  zeroRttHandshake?: boolean | null;
  psk?: string | null;
  version?: number | null;
  user?: string | null;
  privateKey?: string | null;
  privateKeyPassphrase?: string | null;
  peerPublicKey?: string | null;
  localAddress?: string | null;
  preSharedKey?: string | null;
  mtu?: number | null;
  quic?: boolean | null;
  executablePath?: string | null;
  tls?: boolean | null;
  sni?: string | null;
  insecure?: boolean | null;
  alpn?: string | null;
  fingerprint?: string | null;
  realityPublicKey?: string | null;
  realityShortId?: string | null;
  network?: string | null;
  host?: string | null;
  serviceName?: string | null;
  udp?: boolean | null;
}

/** Clash-style routing mode. */
export type OutboundMode = "rule" | "global" | "direct";

/** Extra sing-box inbound listener (settings-managed). */
export interface ExtraInbound {
  id: string;
  /** mixed | http */
  kind: "mixed" | "http";
  port: number;
  /** true → listen 0.0.0.0, false → 127.0.0.1 */
  allow_lan?: boolean;
}

export interface AppSettings {
  mixed_port: number;
  /** Main mixed inbound listens on 0.0.0.0 instead of 127.0.0.1. */
  allow_lan?: boolean;
  api_port: number;
  /** Additional mixed/http listeners emitted into the generated config. */
  extra_inbounds?: ExtraInbound[];
  current_node_id?: string | null;
  clash_api_secret?: string | null;
  /** Gate for clash_api_secret; off by default. */
  api_secret_enabled?: boolean;
  probe_url: string;
  /** Multi-subscription enable (Mix). */
  mix_mode?: boolean;
  /** sing-box TUN inbound (global capture). */
  tun_enabled?: boolean;
  /** Persisted traffic capture preference. */
  capture_mode?: "off" | "system" | "tun";
  /** system | gvisor | mixed */
  tun_stack?: string;
  /** Include an IPv6 address on the TUN interface. Off by default — most
   * nodes have no v6 egress and a dual-stack tun makes Chrome prefer AAAA/v6,
   * black-holing every connection. */
  tun_ipv6_enabled?: boolean;
  /** Reject sniffed QUIC (UDP/443) so browsers fall back to TCP. Off by
   * default. */
  block_quic?: boolean;
  /** Bypass localhost and LAN segments with built-in direct rules. On by
   * default; applied in Rule mode only, after the rule sets. */
  bypass_lan?: boolean;
  /** rule | global | direct */
  outbound_mode?: OutboundMode;
  /** route.final in Rule mode: proxy | direct | block */
  route_final?: "proxy" | "direct" | "block" | string;
  /** Close window → tray (keep process + core). */
  close_to_tray?: boolean;
  /** Launch at OS login. */
  launch_at_login?: boolean;
  /** Start without showing main window. */
  silent_start?: boolean;
  /** Auto-start proxy after app launch. */
  auto_start_proxy?: boolean;
  /** Close all connections when switching node. */
  close_connections_on_switch?: boolean;
  /** UI language: zh | en (sidebar stays English). */
  locale?: string;
  /** UI theme: aerospace | day */
  theme?: string;
  /** UI accent (brand/primary color) preset id, e.g. green | blue | purple ... */
  accent?: string;
  /** Background glow color: "accent" (follow) | preset id | #rrggbb */
  glow_color?: string;
  /** Overview hero visual: particle | classic | smiley */
  hero_style?: HeroStyle;
  /** Frosted-glass look for repeated glass controls (costs backdrop-filter
   * GPU layers; default off = solid fills). */
  glass_frost?: boolean;
  /** Tray mark: badge | mark | ghost | buddy */
  tray_icon?: TrayIconStyle;
  /** Destroy WebView when closing to tray (free GPU/JS; tray+core stay). */
  unload_ui_on_tray?: boolean;
  /** off | smart | kernel — node auto-select mode. */
  auto_select?: AutoSelectMode;
  /** Resolve originating process per connection (sing-box find_process_mode). */
  find_process?: boolean;
  /** @deprecated derived from auto_select === "smart" */
  smart_switch?: boolean;
  /** `generated` or `singbox:<profile_id>`. */
  runtime_source?: string;
  /** Which core runs: `singbox` (default) | `xray` | `mihomo`. */
  core_type?: CoreKind;
}

/** Kernel binary kind. */
export type CoreKind = "singbox" | "xray" | "mihomo";

/** Manual / app smart switch / sing-box urltest. */
export type AutoSelectMode = "off" | "smart" | "kernel";

export type ThemeId = "aerospace" | "day";

export type HeroStyle = "particle" | "radiance" | "classic" | "smiley";

export type TrayIconStyle = "badge" | "mark" | "ghost" | "buddy" | "danger" | "danger2" | "ghost2" | "faceid";

export interface GenerateConfigResult {
  path: string;
  selected_tag: string;
  outbound_count: number;
  mixed_port: number;
  api_port: number;
  preview: string;
}

export interface CoreInfo {
  /** `singbox` | `xray` | `mihomo`. */
  kind: CoreKind;
  /** Display name: sing-box / Xray. */
  name: string;
  installed: boolean;
  version?: string | null;
  path?: string | null;
  platform: string;
  latest_version?: string | null;
  update_available: boolean;
  /** bundled | downloaded | missing */
  source: string;
  bundled_version?: string | null;
}

export interface CoreDownloadResult {
  kind?: CoreKind | string;
  version: string;
  path: string;
  asset_name: string;
  platform: string;
  bytes: number;
}

export interface CoreDownloadProgress {
  kind?: CoreKind | string;
  stage: "preparing" | "downloading" | "installing" | "done";
  downloaded: number;
  total?: number | null;
  percent?: number | null;
  via_proxy: boolean;
}

export type CoreState =
  | "stopped"
  | "starting"
  | "running"
  | "stopping"
  | "error";

/** One detected network issue (detection only — the app never auto-applies a fix). */
export interface DiagnosticIssue {
  id: string;
  issue: string;
  suggestion: string;
}

export interface NetworkDiagnosticsResult {
  issues: DiagnosticIssue[];
}

export interface ProxyStatus {
  running: boolean;
  core_state: CoreState;
  system_proxy: boolean;
  tun_enabled: boolean;
  /** Persisted desired traffic capture mode. */
  capture_mode?: "off" | "system" | "tun";
  /** rule | global | direct */
  outbound_mode: string;
  mixed_port: number;
  api_port: number;
  current_node_id?: string | null;
  error?: string | null;
  core_path?: string | null;
  config_path?: string | null;
  upload_speed: number;
  download_speed: number;
  upload_total: number;
  download_total: number;
  connections: number;
  /** @deprecated use auto_select === "smart" */
  smart_switch?: boolean;
  /** off | smart | kernel */
  auto_select?: AutoSelectMode | string;
  /** Unix seconds when core last started (uptime = now - this). */
  core_started_at?: number | null;
  /** `generated` or `singbox`. */
  runtime_source?: string;
  runtime_profile_id?: string | null;
  runtime_profile_name?: string | null;
  custom_has_clash_api?: boolean;
  custom_has_tun?: boolean;
  custom_inbound_port?: number | null;
  /** Resident memory (bytes) of the core process, when known. */
  core_memory_bytes?: number | null;
  /** Which core is active: `singbox` (default) | `xray` | `mihomo`. */
  core_type?: CoreKind | string;
  /** True when the running core has elevated privileges (macOS: setuid-root;
   *  Windows: UAC). */
  core_elevated?: boolean;
}

export type RuleType =
  | "domain"
  | "domain_suffix"
  | "domain_keyword"
  | "ip_cidr"
  | "process"
  | "geoip";

export interface RuleSetSummary {
  id: string;
  name: string;
  builtin: boolean;
  rule_count: number;
  /** Multiple sets can be enabled and merged for routing. */
  enabled: boolean;
  ownership: "builtin" | "user" | "system";
  strategy: RuleSetStrategy;
  /** Set-level route parameters (strategy === "node" | "filter" | "chain"). */
  node_id?: string | null;
  node_name?: string | null;
  smart_include?: string[];
  smart_exclude?: string[];
  /** When strategy is `chain`: whole-set chain id. */
  chain_id?: string | null;
  chain_name?: string | null;
  dns_strategy: RuleSetDnsStrategy;
  /** Restorable by Reset: only the bundled remote rule sets. */
  resettable: boolean;
  remote?: RemoteRuleSetConfig | null;
}

/** Whole-set route strategies. `node` (pinned node), `filter`
 *  (keyword-filtered pool), and `chain` (multi-hop chain) are whole-set pins
 *  whose parameters live on the set level; `smart` (Mixed) keeps per-rule
 *  decisions. */
export type RuleSetStrategy =
  | "proxy"
  | "direct"
  | "block"
  | "node"
  | "filter"
  | "chain"
  | "smart";
export type RuleSetDnsStrategy = "local" | "domestic" | "remote";

export interface RemoteRuleSetConfig {
  url: string;
  format: "source" | "binary" | string;
  update_interval: "disabled" | "1h" | "12h" | "24h" | string;
  /** Whole-set route; `node`/`smart` use the set-level pin / filters. */
  target: RuleTarget;
  local_path?: string | null;
  download_status?: "idle" | "downloading" | "ready" | "error" | string;
  download_error?: string | null;
  last_update?: number | null;
  last_attempt?: number | null;
  rule_count?: number | null;
}

export interface RemoteRuleItem {
  index: number;
  kind: string;
  summary: string;
  raw: string;
  raw_truncated: boolean;
  complex: boolean;
}

export interface RemoteRulePage {
  total: number;
  offset: number;
  limit: number;
  items: RemoteRuleItem[];
}

export interface RuleSet {
  id: string;
  name: string;
  builtin: boolean;
  enabled: boolean;
  ownership: "builtin" | "user" | "system";
  strategy: RuleSetStrategy;
  /** When strategy is `node`: whole-set pinned node id. */
  node_id?: string | null;
  /** Snapshot name at pin time (stale UI when id missing). */
  node_name?: string | null;
  /** When strategy is `filter`: whitelist keywords (OR). Empty = all nodes. */
  smart_include?: string[];
  /** When strategy is `filter`: blacklist keywords (OR). */
  smart_exclude?: string[];
  /** When strategy is `chain`: whole-set chain id. */
  chain_id?: string | null;
  /** Snapshot name at pin time (stale UI when id missing). */
  chain_name?: string | null;
  dns_strategy: RuleSetDnsStrategy;
  remote?: RemoteRuleSetConfig | null;
  dns_rules: DnsRule[];
  rules: Rule[];
}

export type RuleTarget = "direct" | "proxy" | "block" | "node" | "smart" | "chain";

export interface Rule {
  id: string;
  ord: number;
  type: RuleType;
  payload: string;
  target: RuleTarget;
  enabled: boolean;
  /** When target is `node`: pinned subscription node id. */
  node_id?: string | null;
  /** Snapshot name at save time (stale UI when id missing). */
  node_name?: string | null;
  /** Smart mode whitelist: name must contain any keyword (OR). Empty = no whitelist. */
  smart_include?: string[];
  /** Smart mode blacklist: name containing any keyword is skipped (OR). */
  smart_exclude?: string[];
  /** When target is `chain`: chain id to route through. */
  chain_id?: string | null;
  /** Snapshot name at save time (stale UI when id missing). */
  chain_name?: string | null;
}

/** How a [[NodePool]] selects its member nodes. */
export type PoolMode =
  | { mode: "explicit"; node_ids: string[] }
  | { mode: "keyword"; include: string[]; exclude: string[] };

/** Named, reusable node pool — referenced by chain hops and by
 *  Rule/RuleSet `chain` targets (via the chain's hops), so multiple
 *  chains can share one pool definition. */
export interface NodePool {
  id: string;
  name: string;
  mode: PoolMode;
}

/** One hop in a [[ProxyChain]] — either a single pinned node or a pool
 *  (resolved to that pool's selector/urltest outbound at build time). */
export type ChainHop =
  | { kind: "node"; node_id: string }
  | { kind: "pool"; pool_id: string };

/** Named, ordered chain of hops, entry point first. Built into a sing-box
 *  `detour` chain: hop[0]'s outbound detours into hop[1], hop[1] into
 *  hop[2], … the last hop exits directly. Must have at least 2 hops. */
export interface ProxyChain {
  id: string;
  name: string;
  hops: ChainHop[];
}

/** One hop's chain-diagnosis probes (see `services/chain_diag.rs`). */
export interface ChainHopDiag {
  label: string;
  kind: "node" | "pool" | string;
  stale: boolean;
  /** Latency of the hop alone (plain node tag / shared pool selector). */
  soloMs?: number | null;
  soloError?: string | null;
  /** Latency through the chain prefix ending at this hop (whole chain for
   *  the last hop — the exit rules point there). */
  chainedMs?: number | null;
  chainedError?: string | null;
}

export interface ChainDiagnosis {
  hops: ChainHopDiag[];
  /** Real-world exit verification. */
  exit: ChainExitProbe;
}

/** Geo/quality facts from api.ip.sb/geoip as seen by the chain's exit. */
export interface ChainExitGeo {
  ip: string;
  country?: string | null;
  countryCode?: string | null;
  region?: string | null;
  city?: string | null;
  asn?: string | null;
  asnOrganization?: string | null;
  organization?: string | null;
  timezone?: string | null;
}

export interface ChainExitProbe {
  geo?: ChainExitGeo | null;
  ipError?: string | null;
  /** Real HTTPS round-trip to https://api.ip.sb/ip through the whole chain. */
  ipSbMs?: number | null;
  ipSbError?: string | null;
}

/** Live connection or historical request row */
export interface ConnectionView {
  id: string;
  destination: string;
  host: string;
  network: string;
  conn_type: string;
  node_tag: string;
  node_name: string;
  /** Owning subscription name (for tooltip). */
  subscription_name?: string;
  chains: string[];
  chains_display: string;
  rule: string;
  rule_payload: string;
  process: string;
  source: string;
  upload: number;
  download: number;
  start: string;
  first_seen?: number | null;
  last_seen?: number | null;
  closed?: boolean;
  closed_at?: number | null;
}

export interface LiveConnectionBatch {
  rows: ConnectionView[];
  removed_ids: string[];
  /** Full id order. Omitted when membership is unchanged since the client's
   * last `order_revision` — then merge `rows` in place without reordering. */
  order_ids?: string[] | null;
  order_revision: number;
  revision: number;
  unchanged: boolean;
  full: boolean;
}
