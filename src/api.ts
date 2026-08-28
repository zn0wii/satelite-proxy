import { invoke } from "@tauri-apps/api/core";
import type {
  AppSettings,
  ChainDiagnosis,
  ChainHop,
  CoreDownloadResult,
  CoreInfo,
  CoreKind,
  GenerateConfigResult,
  ImportResult,
  LatencyBatchResult,
  ConnectionView,
  NodePool,
  PoolMode,
  ProxyChain,
  ProxyNode,
  ProxyStatus,
  Rule,
  RuleSet,
  RuleSetStrategy,
  RuleSetDnsStrategy,
  RuleSetSummary,
  RuleTarget,
  RuleType,
  SubscriptionDetail,
  SubscriptionUrlEntry,
  SubscriptionView,
  DnsSettings,
  DnsTestResult,
  HostsEntry,
} from "./types";
import { trackCoreBusy } from "./coreBusy";

export function listSubscriptions() {
  return invoke<SubscriptionView[]>("list_subscriptions");
}

export function listSubscriptionUrls() {
  return invoke<SubscriptionUrlEntry[]>("list_subscription_urls");
}

export function getSubscription(id: string) {
  return invoke<SubscriptionDetail>("get_subscription", { id });
}

export function addSubscriptionUrl(
  name: string | null,
  url: string,
  viaProxy = false,
  autoUpdate = false,
  autoUpdateIntervalMin = 1440,
) {
  return invoke<ImportResult>("add_subscription_url", {
    name,
    url,
    viaProxy,
    autoUpdate,
    autoUpdateIntervalMin,
  });
}

export function addSubscriptionFile(
  name: string | null,
  path: string,
  autoUpdate = false,
  autoUpdateIntervalMin = 1440,
) {
  return invoke<ImportResult>("add_subscription_file", {
    name,
    path,
    autoUpdate,
    autoUpdateIntervalMin,
  });
}

export function addSubscriptionText(name: string | null, content: string) {
  return invoke<ImportResult>("add_subscription_text", {
    name,
    content,
  });
}

export function addSubscriptionNode(
  name: string | null,
  uri: string | null,
  node: import("./types").ManualNodeDraft | null,
) {
  return invoke<ImportResult>("add_subscription_node", {
    name,
    uri,
    node,
  });
}

export function addSubscriptionSingbox(
  name: string | null,
  content: string | null,
  path: string | null = null,
) {
  return invoke<ImportResult>("add_subscription_singbox", {
    name,
    content,
    path,
  });
}

export function readImportFile(path: string) {
  return invoke<string>("read_import_file", { path });
}

export function updateSubscription(input: {
  id: string;
  name: string | null;
  kind: "url" | "file" | "text" | "node" | "singbox";
  url?: string | null;
  path?: string | null;
  content?: string | null;
  uri?: string | null;
  node?: import("./types").ManualNodeDraft | null;
  viaProxy?: boolean | null;
  autoUpdate?: boolean | null;
  autoUpdateIntervalMin?: number | null;
}) {
  return invoke<ImportResult>("update_subscription", {
    id: input.id,
    name: input.name,
    kind: input.kind,
    url: input.url ?? null,
    path: input.path ?? null,
    content: input.content ?? null,
    uri: input.uri ?? null,
    node: input.node ?? null,
    viaProxy: input.viaProxy ?? null,
    autoUpdate: input.autoUpdate ?? null,
    autoUpdateIntervalMin: input.autoUpdateIntervalMin ?? null,
  });
}

export function refreshSubscription(id: string, viaProxy?: boolean | null) {
  return invoke<ImportResult>("refresh_subscription", {
    id,
    viaProxy: viaProxy ?? null,
  });
}

export function removeSubscription(id: string) {
  return invoke<void>("remove_subscription", { id });
}

/** Exclusive select / Mix toggle. Returns updated subscription list. */
export function activateSubscription(id: string) {
  return invoke<SubscriptionView[]>("activate_subscription", { id });
}

/** Homepage launch source: `generated` or `singbox:<id>`. Restarts if running. */
export function setRuntimeSource(source: string) {
  return keepSettings(invoke<AppSettings>("set_runtime_source", { source }));
}

export function setMixMode(mix: boolean) {
  return keepSettings(invoke<AppSettings>("set_mix_mode", { mix }));
}

export function listSubscriptionNodes(id: string) {
  return invoke<ProxyNode[]>("list_subscription_nodes", { id });
}

export function listAllNodes() {
  return invoke<ProxyNode[]>("list_all_nodes");
}

export function listNodesPage(query: string, sortMode: string, offset = 0, limit = 200) {
  return invoke<import("./types").NodePage>("list_nodes_page", {
    query,
    sortMode,
    offset,
    limit,
  });
}

export function listNodeIds(query = "") {
  return invoke<string[]>("list_node_ids", { query });
}

/** Read-only nodes extracted from the selected custom sing-box config (custom mode). */
export function listCustomConfigNodes() {
  return invoke<ProxyNode[]>("list_custom_config_nodes");
}

export function renameNode(id: string, name: string) {
  return invoke<ProxyNode>("rename_node", { id, name });
}

/**
 * Cross-mount snapshots of the latest resolved settings / proxy status.
 * Tab switches remount pages (key={nav} page-enter animation), so control
 * state seeded from a default would visibly flip to the persisted value once
 * the mount IPC lands. Pages seed their initial state from these snapshots
 * instead and still refresh from the backend right after mount.
 */
let settingsSnapshot: AppSettings | null = null;
let proxySnapshot: ProxyStatus | null = null;

/** Latest resolved settings, or null before the first get/mutate resolves. */
export function peekSettings(): AppSettings | null {
  return settingsSnapshot;
}

/** Latest resolved proxy status, or null before the first status call. */
export function peekProxyStatus(): ProxyStatus | null {
  return proxySnapshot;
}

function keepSettings(p: Promise<AppSettings>): Promise<AppSettings> {
  return p.then((settings) => {
    settingsSnapshot = settings;
    return settings;
  });
}

function keepProxy(p: Promise<ProxyStatus>): Promise<ProxyStatus> {
  return p.then((status) => {
    proxySnapshot = status;
    return status;
  });
}

export function getSettings() {
  return keepSettings(invoke<AppSettings>("get_settings"));
}

/** Detection-only network diagnostics (e.g. system DNS bypassing TUN).
 * Never mutates system settings. */
export function diagnoseNetwork() {
  return invoke<import("./types").NetworkDiagnosticsResult>("diagnose_network");
}

export interface SettingsUpdatePayload {
  mixedPort?: number | null;
  /** Main mixed inbound listens on 0.0.0.0 (LAN) instead of 127.0.0.1. */
  allowLan?: boolean | null;
  apiPort?: number | null;
  /** Gate for the Clash API secret (off by default). */
  apiSecretEnabled?: boolean | null;
  /** Some(list) replaces the whole extra-inbound list. */
  extraInbounds?: import("./types").ExtraInbound[] | null;
  probeUrl?: string | null;
  tunEnabled?: boolean | null;
  tunStack?: string | null;
  tunIpv6Enabled?: boolean | null;
  blockQuic?: boolean | null;
  /** Bypass localhost and LAN segments with built-in direct rules. */
  bypassLan?: boolean | null;
  closeToTray?: boolean | null;
  launchAtLogin?: boolean | null;
  silentStart?: boolean | null;
  autoStartProxy?: boolean | null;
  closeConnectionsOnSwitch?: boolean | null;
  locale?: string | null;
  theme?: string | null;
  accent?: string | null;
  /** Background glow: "accent" | preset id | #rrggbb */
  glowColor?: string | null;
  homeBackground?: string | null;
  heroStyle?: string | null;
  glassFrost?: boolean | null;
  trayIcon?: string | null;
  unloadUiOnTray?: boolean | null;
  /** @deprecated prefer autoSelect */
  smartSwitch?: boolean | null;
  /** off | smart | kernel */
  autoSelect?: string | null;
  /** proxy | direct | block — route.final in Rule mode */
  routeFinal?: string | null;
  /** Resolve originating process per connection (find_process_mode). */
  findProcess?: boolean | null;
}

type SettingsWaiter = {
  resolve: (settings: AppSettings) => void;
  reject: (error: unknown) => void;
};

let pendingSettings: SettingsUpdatePayload = {};
let pendingSettingsWaiters: SettingsWaiter[] = [];
let settingsTimer: number | null = null;
let settingsWriteInFlight = false;

function scheduleSettingsWrite() {
  if (settingsTimer != null || settingsWriteInFlight || pendingSettingsWaiters.length === 0) return;
  settingsTimer = window.setTimeout(() => {
    settingsTimer = null;
    const payload = pendingSettings;
    const waiters = pendingSettingsWaiters;
    pendingSettings = {};
    pendingSettingsWaiters = [];
    settingsWriteInFlight = true;
    void invoke<AppSettings>("update_settings", {
      mixedPort: payload.mixedPort ?? null,
      allowLan: payload.allowLan ?? null,
      apiPort: payload.apiPort ?? null,
      apiSecretEnabled: payload.apiSecretEnabled ?? null,
      extraInbounds: payload.extraInbounds ?? null,
      probeUrl: payload.probeUrl ?? null,
      tunEnabled: payload.tunEnabled ?? null,
      tunStack: payload.tunStack ?? null,
      tunIpv6Enabled: payload.tunIpv6Enabled ?? null,
      blockQuic: payload.blockQuic ?? null,
      bypassLan: payload.bypassLan ?? null,
      closeToTray: payload.closeToTray ?? null,
      launchAtLogin: payload.launchAtLogin ?? null,
      silentStart: payload.silentStart ?? null,
      autoStartProxy: payload.autoStartProxy ?? null,
      closeConnectionsOnSwitch: payload.closeConnectionsOnSwitch ?? null,
      locale: payload.locale ?? null,
      theme: payload.theme ?? null,
      accent: payload.accent ?? null,
      glowColor: payload.glowColor ?? null,
      homeBackground: payload.homeBackground ?? null,
      heroStyle: payload.heroStyle ?? null,
      glassFrost: payload.glassFrost ?? null,
      trayIcon: payload.trayIcon ?? null,
      unloadUiOnTray: payload.unloadUiOnTray ?? null,
      smartSwitch: payload.smartSwitch ?? null,
      autoSelect: payload.autoSelect ?? null,
      routeFinal: payload.routeFinal ?? null,
      findProcess: payload.findProcess ?? null,
    })
      .then((settings) => {
        settingsSnapshot = settings;
        waiters.forEach(({ resolve }) => resolve(settings));
      })
      .catch((error) => waiters.forEach(({ reject }) => reject(error)))
      .finally(() => {
        settingsWriteInFlight = false;
        scheduleSettingsWrite();
      });
  }, 60);
}

/** Merge settings changes made in the same interaction burst into one durable write. */
export function updateSettings(payload: SettingsUpdatePayload) {
  pendingSettings = { ...pendingSettings, ...payload };
  const result = new Promise<AppSettings>((resolve, reject) => {
    pendingSettingsWaiters.push({ resolve, reject });
  });
  scheduleSettingsWrite();
  return result;
}

/** Rotate the clash_api secret (user-triggered). Restarts a running core. */
export function regenerateApiSecret() {
  return keepSettings(invoke<AppSettings>("regenerate_api_secret"));
}

export function setCurrentNode(nodeId: string) {
  return keepSettings(invoke<AppSettings>("set_current_node", { nodeId }));
}

export interface SmartSwitchNowResult {
  switched: boolean;
  from_id?: string | null;
  to_id?: string | null;
  to_name?: string | null;
  latency_ms?: number | null;
  probed: number;
  message: string;
}

/** Probe candidates and switch to best node (used when enabling smart switch). */
export function smartSwitchNow() {
  return invoke<SmartSwitchNowResult>("smart_switch_now");
}

export type AppLogLevel = "trace" | "debug" | "info" | "warn" | "error";

export interface AppLogEntry {
  id: number;
  ts_ms: number;
  level: AppLogLevel;
  target: string;
  message: string;
}

export interface AppLogBatch {
  entries: AppLogEntry[];
  cursor: number;
}

export function listAppLogs(opts?: {
  minLevel?: AppLogLevel | null;
  limit?: number | null;
  query?: string | null;
  afterId?: number | null;
}) {
  return invoke<AppLogBatch>("list_app_logs", {
    minLevel: opts?.minLevel ?? "info",
    limit: opts?.limit ?? 500,
    query: opts?.query ?? null,
    afterId: opts?.afterId ?? null,
  });
}

export function clearAppLogs() {
  return invoke<void>("clear_app_logs");
}

export interface CoreLogTail {
  /** Absolute path of the core's hourly log file, when a session exists. */
  path: string | null;
  lines: string[];
}

/** Tail of the active core's log (Xray-mode traffic page stand-in). */
export function getCoreLogTail(limit?: number | null) {
  return invoke<CoreLogTail>("get_core_log_tail", { limit: limit ?? null });
}

export function generateSingboxConfig() {
  return invoke<GenerateConfigResult>("generate_singbox_config");
}

export function previewSingboxConfig() {
  return invoke<GenerateConfigResult>("preview_singbox_config");
}

export function getActiveConfigPath() {
  return invoke<string | null>("get_active_config_path");
}

/** Local only — no network. Use for first paint. `kind` defaults to singbox. */
export function getCoreInfo(kind?: CoreKind | null) {
  return invoke<CoreInfo>("get_core_info", { kind: kind ?? null });
}

/** Machine's LAN IPv4 (default-route interface). Null when offline. */
export function getLanIp() {
  return invoke<string | null>("get_lan_ip");
}

export function checkCoreUpdate(kind: CoreKind | null, localVersion?: string | null) {
  return invoke<{
    kind: string;
    latest_version: string;
    update_available: boolean;
    asset_name: string;
    size: number;
  }>("check_core_update", { kind, localVersion: localVersion ?? null });
}

/** Latest app release tag from GitHub; routes via the running proxy.
 * `force` bypasses the 6h local cache (manual check button); auto checks
 * on tab open reuse a fresh cached result to spare the API quota. */
export function checkAppUpdate(force = false) {
  return invoke<{
    current_version: string;
    latest_version: string;
    update_available: boolean;
    cached: boolean;
    checked_at: number | null;
  }>("check_app_update", { force });
}

/** Absolute path of the app's own executable (install location). */
export function getAppInstallPath() {
  return invoke<string>("get_app_install_path");
}

export function downloadCore(kind?: CoreKind | null, tag?: string | null) {
  return invoke<CoreDownloadResult>("download_core", {
    kind: kind ?? null,
    tag: tag ?? null,
  });
}

export function fetchCoreLatest(kind?: CoreKind | null) {
  return invoke<{
    version: string;
    asset_name: string;
    download_url: string;
    size: number;
    platform: string;
  }>("fetch_core_latest", { kind: kind ?? null });
}

/** Switch the active core (singbox | xray | mihomo). Restarts a running core. */
export function setCoreType(kind: CoreKind) {
  return invoke<AppSettings>("set_core_type", { kind });
}

export interface GeodataFileInfo {
  present: boolean;
  bytes: number;
  modified_at: number | null;
}

export interface GeodataInfo {
  geosite: GeodataFileInfo;
  geoip: GeodataFileInfo;
}

/** Kernel geodata state; `force` re-downloads first. `kind` selects the
 * pair: "xray" (Loyalsoldier .dat) or "mihomo" (MetaCubeX mmdb + mrs). */
export function refreshGeodata(force = false, kind: CoreKind | null = null) {
  return invoke<GeodataInfo>("refresh_geodata", { force, kind });
}

export function testNodesLatency(ids?: string[] | null, timeoutMs?: number | null) {
  // Tauri 2 accepts camelCase; include snake_case for compatibility.
  const args: Record<string, unknown> = {
    ids: ids ?? null,
    timeoutMs: timeoutMs ?? null,
    timeout_ms: timeoutMs ?? null,
  };
  return invoke<LatencyBatchResult>("test_nodes_latency", args);
}

/** Same TCP probe, for nodes extracted from the selected custom sing-box config (results not persisted). */
export function testCustomNodesLatency(timeoutMs?: number | null) {
  const args: Record<string, unknown> = {
    timeoutMs: timeoutMs ?? null,
    timeout_ms: timeoutMs ?? null,
  };
  return invoke<LatencyBatchResult>("test_custom_nodes_latency", args);
}

export function getProxyStatus() {
  return keepProxy(invoke<ProxyStatus>("get_proxy_status"));
}

export function startProxy(enableSystemProxy = false) {
  return keepProxy(
    trackCoreBusy(
      invoke<ProxyStatus>("start_proxy", {
        enableSystemProxy,
      }),
    ),
  );
}

export function stopProxy() {
  return keepProxy(trackCoreBusy(invoke<ProxyStatus>("stop_proxy")));
}

export function restartProxy() {
  // Slightly longer min hold so ⋯ / Overview restart never flash-clears.
  return keepProxy(trackCoreBusy(invoke<ProxyStatus>("restart_proxy"), 700));
}

export function setSystemProxy(enabled: boolean) {
  return keepProxy(invoke<ProxyStatus>("set_system_proxy", { enabled }));
}

/** Toggle TUN; restarts core when running so config applies. */
export function setTunEnabled(enabled: boolean) {
  return keepProxy(
    trackCoreBusy(invoke<ProxyStatus>("set_tun_enabled", { enabled })),
  );
}

/** Traffic capture mode: off | system | tun (mutually exclusive). */
export function setCaptureMode(mode: "off" | "system" | "tun") {
  return keepProxy(
    trackCoreBusy(invoke<ProxyStatus>("set_capture_mode", { mode })),
  );
}

/** rule | global | direct — restarts core when running. */
export function setOutboundMode(mode: "rule" | "global" | "direct") {
  return keepProxy(
    trackCoreBusy(invoke<ProxyStatus>("set_outbound_mode", { mode })),
  );
}

export function getDnsSettings() {
  return invoke<DnsSettings>("get_dns_settings");
}

export function updateDnsSettings(settings: DnsSettings, apply = true) {
  return invoke<DnsSettings>("update_dns_settings", { settings, apply });
}

/** Reset DNS servers or rules to factory defaults (`"servers"` | `"rules"`). */
export function resetDnsDefaults(section: "servers" | "rules", apply = true) {
  return invoke<DnsSettings>("reset_dns_defaults", { section, apply });
}

export function testDnsLookup(domain: string) {
  return invoke<DnsTestResult>("test_dns_lookup", { domain });
}

/** Read the OS hosts file as a read-only entry list (for the Hosts UI). */
export function readSystemHosts() {
  return invoke<HostsEntry[]>("read_system_hosts");
}

export function listRuleSets() {
  return invoke<RuleSetSummary[]>("list_rule_sets");
}

export function getRuleSet(id: string) {
  return invoke<RuleSet>("get_rule_set", { id });
}

export function setActiveRuleSet(id: string) {
  return invoke<void>("set_active_rule_set", { id });
}

export function setRuleSetEnabled(id: string, enabled: boolean) {
  return invoke<void>("set_rule_set_enabled", { id, enabled });
}

export function setRuleSetStrategy(id: string, strategy: RuleSetStrategy) {
  return invoke<RuleSet>("set_rule_set_strategy", { id, strategy });
}

export function setRuleSetDnsStrategy(id: string, strategy: RuleSetDnsStrategy) {
  return invoke<RuleSet>("set_rule_set_dns_strategy", { id, strategy });
}

export function createRuleSet(
  name: string,
  remoteUrl?: string | null,
  target?: RuleTarget | null,
  updateInterval?: "disabled" | "1h" | "12h" | "24h" | null,
  nodeId?: string | null,
  smartInclude?: string[] | null,
  smartExclude?: string[] | null,
  chainId?: string | null,
) {
  return invoke<RuleSet>("create_rule_set", {
    name,
    remoteUrl: remoteUrl ?? null,
    target: target ?? null,
    updateInterval: updateInterval ?? null,
    nodeId: nodeId ?? null,
    smartInclude: smartInclude ?? null,
    smartExclude: smartExclude ?? null,
    chainId: chainId ?? null,
  });
}

export function refreshRemoteRuleSet(id: string) {
  return invoke<RuleSet>("refresh_remote_rule_set", { id });
}

export function updateRuleSet(
  id: string,
  name: string,
  remoteUrl?: string | null,
  updateInterval?: "disabled" | "1h" | "12h" | "24h" | null,
) {
  return invoke<RuleSet>("update_rule_set", {
    id,
    name,
    remoteUrl: remoteUrl ?? null,
    updateInterval: updateInterval ?? null,
  });
}

/** Apply one target to every rule of a local set (batch set-routes). */
export function batchSetRuleTargets(
  id: string,
  target: "proxy" | "direct" | "block" | "node" | "smart" | "chain",
  nodeId?: string | null,
  smartInclude?: string[] | null,
  smartExclude?: string[] | null,
  chainId?: string | null,
) {
  return invoke<RuleSet>("batch_set_rule_targets", {
    id,
    target,
    nodeId: nodeId ?? null,
    smartInclude: smartInclude ?? null,
    smartExclude: smartExclude ?? null,
    chainId: chainId ?? null,
  });
}

export function listRemoteRuleItems(
  id: string,
  offset: number,
  limit: number,
  query?: string,
) {
  return invoke<import("./types").RemoteRulePage>("list_remote_rule_items", {
    id,
    offset,
    limit,
    query: query?.trim() || null,
  });
}

/** First id = highest match priority. Restarts core when running. */
export function reorderRuleSets(ids: string[]) {
  return invoke<RuleSetSummary[]>("reorder_rule_sets", { ids });
}

export function deleteRuleSet(id: string) {
  return invoke<void>("delete_rule_set", { id });
}

/** Reset one bundled remote rule set to factory settings. */
export function resetRuleSet(id: string) {
  return invoke<RuleSet>("reset_rule_set", { id });
}

/** Reset the three bundled remote rule sets (geosite-cn / geoip-cn / geolocation-!cn). */
export function resetBuiltinRuleSet() {
  return invoke<RuleSet>("reset_builtin_rule_set");
}

export function listRules(setId?: string | null) {
  return invoke<Rule[]>("list_rules", { setId: setId ?? null });
}

export function saveRule(input: {
  setId?: string | null;
  id?: string | null;
  ruleType: RuleType;
  payload: string;
  target: RuleTarget;
  ord?: number | null;
  enabled?: boolean | null;
  nodeId?: string | null;
  smartInclude?: string[] | null;
  smartExclude?: string[] | null;
  chainId?: string | null;
}) {
  return invoke<Rule>("save_rule", {
    input: {
      set_id: input.setId ?? null,
      id: input.id ?? null,
      rule_type: input.ruleType,
      payload: input.payload,
      target: input.target,
      ord: input.ord ?? null,
      enabled: input.enabled ?? null,
      node_id: input.nodeId ?? null,
      smart_include: input.smartInclude ?? null,
      smart_exclude: input.smartExclude ?? null,
      chain_id: input.chainId ?? null,
    },
  });
}

export function removeRule(id: string, setId?: string | null) {
  return invoke<void>("remove_rule", { id, setId: setId ?? null });
}

export function setRuleEnabled(id: string, enabled: boolean, setId?: string | null) {
  return invoke<Rule>("set_rule_enabled", {
    id,
    enabled,
    setId: setId ?? null,
  });
}

// ---- Proxy Chain: node pools + multi-hop chains --------------------------

export function listPools() {
  return invoke<NodePool[]>("list_pools");
}

export function createPool(name: string, mode: PoolMode) {
  return invoke<NodePool>("create_pool", { name, mode });
}

export function updatePool(id: string, name: string, mode: PoolMode) {
  return invoke<NodePool>("update_pool", { id, name, mode });
}

export function deletePool(id: string) {
  return invoke<void>("delete_pool", { id });
}

export function listChains() {
  return invoke<ProxyChain[]>("list_chains");
}

/** Rule-set names referencing each chain (set-level pin or any single rule),
 *  keyed by chain id — same detection the delete guard uses. */
export function listChainUsage() {
  return invoke<Record<string, string[]>>("list_chain_usage");
}

export function createChain(name: string, hops: ChainHop[]) {
  return invoke<ProxyChain>("create_chain", { name, hops });
}

export function updateChain(id: string, name: string, hops: ChainHop[]) {
  return invoke<ProxyChain>("update_chain", { id, name, hops });
}

export function deleteChain(id: string) {
  return invoke<void>("delete_chain", { id });
}

/** Probe each chain hop solo + through the chain prefix (sing-box only). */
export function diagnoseChain(chainId: string) {
  return invoke<ChainDiagnosis>("diagnose_chain", { chainId });
}

export function listConnections() {
  return invoke<ConnectionView[]>("list_connections");
}

export function listConnectionChanges(
  sinceRevision?: number | null,
  lastOrderRevision?: number | null,
) {
  return invoke<import("./types").LiveConnectionBatch>("list_connection_changes", {
    sinceRevision: sinceRevision ?? null,
    lastOrderRevision: lastOrderRevision ?? null,
  });
}

export interface RequestBatch {
  entries: ConnectionView[];
  cursor: number;
}

export function listRequests(
  query?: string | null,
  limit?: number | null,
  afterSeq?: number | null,
) {
  return invoke<RequestBatch>("list_requests", {
    query: query ?? null,
    limit: limit ?? null,
    afterSeq: afterSeq ?? null,
  });
}

/** Suspicious closed requests: short-lived & near-zero bytes (failure/timeout). */
export function listRequestFailures(
  query?: string | null,
  limit?: number | null,
  afterSeq?: number | null,
) {
  return invoke<RequestBatch>("list_request_failures", {
    query: query ?? null,
    limit: limit ?? null,
    afterSeq: afterSeq ?? null,
  });
}

export function clearRequestHistory() {
  return invoke<void>("clear_request_history");
}
