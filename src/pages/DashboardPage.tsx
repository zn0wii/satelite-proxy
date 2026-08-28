import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { getVersion } from "@tauri-apps/api/app";
import {
  getCoreInfo,
  getLanIp,
  getProxyStatus,
  getSettings,
  getSubscription,
  listAllNodes,
  listSubscriptions,
  peekProxyStatus,
  previewSingboxConfig,
  restartProxy,
  peekSettings,
  setCoreType,
  setRuntimeSource,
  setOutboundMode,
  startProxy,
  smartSwitchNow,
  stopProxy,
  testNodesLatency,
  updateSettings,
} from "../api";
import {
  useCaptureModeSwitch,
  type CaptureMode,
} from "../hooks/useCaptureModeSwitch";
import { useVisibleInterval } from "../hooks/useVisibleInterval";
import { isZoomSettling } from "../hooks/viewportScale";
import { useI18n } from "../i18n";
import { ErrorModal } from "../components/ErrorModal";
import { GlassButton } from "../components/GlassButton";
import { GlassSeg } from "../components/GlassSeg";
import { EarthGlobeLazy } from "../components/EarthGlobeLazy";
import { HeroVisual } from "../components/HeroVisual";
import { useTheme } from "../theme";
import { SimpleTrafficSpark } from "../ui/simple/SimpleTrafficSpark";
import type {
  AutoSelectMode,
  CoreKind,
  GenerateConfigResult,
  OutboundMode,
  ProxyNode,
  ProxyStatus,
  SubscriptionTraffic,
  SubscriptionView,
} from "../types";

/**
 * Split a config line around (all occurrences of) the filter string and wrap
 * the hits in <mark>. Case-insensitive; the query is always non-empty here.
 */
function highlightPreviewLine(line: string, query: string): ReactNode {
  const lower = line.toLowerCase();
  const lq = query.toLowerCase();
  const parts: ReactNode[] = [];
  let from = 0;
  for (;;) {
    const at = lower.indexOf(lq, from);
    if (at === -1) {
      parts.push(line.slice(from));
      return parts;
    }
    if (at > from) parts.push(line.slice(from, at));
    parts.push(<mark key={at}>{line.slice(at, at + query.length)}</mark>);
    from = at + query.length;
  }
}

/**
 * Detects the backend's fixed-text TUN-permission hint (see
 * `map_tun_permission_hint` in `core/manager.rs`) so the error modal can
 * offer a one-click "reauthorize and retry" action instead of leaving the
 * user to guess which switch fixes a setuid/UAC failure. Matches on the
 * hint's own anchor phrases — not on generic "permission denied" — since
 * those phrases are appended only for this exact failure class.
 */
function isTunPermissionError(msg: string): boolean {
  return (
    msg.includes("TUN 需要更高权限才能创建虚拟网卡") ||
    msg.includes("TUN 模式需要管理员权限以创建虚拟网卡")
  );
}

interface Props {
  onGoProfiles?: () => void;
  onGoNodes?: () => void;
  onGoTraffic?: () => void;
  onGoSettings?: () => void;
}

function fmtSpeed(bps: number) {
  if (bps < 1024) return `${bps} B/s`;
  if (bps < 1024 * 1024) return `${(bps / 1024).toFixed(1)} KB/s`;
  return `${(bps / (1024 * 1024)).toFixed(2)} MB/s`;
}

function fmtBytes(n: number) {
  if (!Number.isFinite(n) || n < 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB", "PB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i++;
  }
  if (i === 0) return `${Math.round(v)} B`;
  const text = v >= 100 ? String(Math.round(v)) : v.toFixed(1);
  return `${text} ${units[i]}`;
}

function quotaParts(tr: SubscriptionTraffic | null | undefined) {
  if (!tr) return null;
  const total = tr.total != null && tr.total > 0 ? tr.total : null;
  const remaining =
    tr.quota_remaining != null && tr.quota_remaining >= 0
      ? tr.quota_remaining
      : null;
  if (total == null && remaining == null) return null;
  const usedParts = (tr.upload ?? 0) + (tr.download ?? 0);
  const used =
    total != null && usedParts === 0 && remaining != null
      ? Math.max(0, total - remaining)
      : usedParts;
  return { used, total, remaining };
}

function fmtLatency(ms?: number | null) {
  if (ms == null || ms < 0) return "—";
  return `${ms} ms`;
}

function latencyClass(ms?: number | null) {
  if (ms == null || ms < 0) return "lat-none";
  if (ms < 200) return "lat-good";
  if (ms < 300) return "lat-ok";
  return "lat-slow";
}

/** Uptime from core_started_at (unix secs) to now, as "HH:MM:SS". */
function fmtUptime(startedAt?: number | null) {
  if (startedAt == null) return "—";
  const secs = Math.max(0, Math.floor(Date.now() / 1000 - startedAt));
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  return [h, m, s].map((n) => String(n).padStart(2, "0")).join(":");
}

/** Keep an element on a single line by shrinking its font until the content
 *  fits (never wraps; ellipsis is the last resort below the size floor).
 *  Re-fits when the text changes or the window resizes. */
function useSingleLineFit<T extends HTMLElement>(text: string) {
  const ref = useRef<T | null>(null);
  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    const fit = () => {
      // Root-zoom transitions (maximize magnification) skew computed
      // font-size and clientWidth mid-animation — the written inline size
      // would persist after the animation. Skip while settling; the
      // at-rest synthetic resize (viewportScale.ts) re-fits correctly.
      if (isZoomSettling()) return;
      // Drop any previous override so the CSS clamp() sets the start size.
      el.style.fontSize = "";
      let size = parseFloat(getComputedStyle(el).fontSize);
      while (size > 12 && el.scrollWidth > el.clientWidth) {
        size -= 1;
        el.style.fontSize = `${size}px`;
      }
    };
    fit();
    window.addEventListener("resize", fit);
    return () => window.removeEventListener("resize", fit);
  }, [text]);
  return ref;
}

export function DashboardPage({
  onGoProfiles,
  onGoNodes,
  onGoTraffic,
}: Props) {
  const { t } = useI18n();
  const { heroIsGlobe } = useTheme();
  const [subs, setSubs] = useState<SubscriptionView[]>([]);
  const [nodes, setNodes] = useState<ProxyNode[]>([]);
  const [currentNode, setCurrentNode] = useState<ProxyNode | null>(null);
  /** settings.current_node_id — available before full node list. */
  const [currentNodeId, setCurrentNodeId] = useState<string | null>(null);
  const [settingsPorts, setSettingsPorts] = useState({
    mixed: 2080,
    api: 19090,
    extras: [] as import("../types").ExtraInbound[],
  });
  /** LAN IPv4 for the listen card (null until fetched / offline). */
  const [lanIp, setLanIp] = useState<string | null>(null);
  const [coreVersion, setCoreVersion] = useState<string | null>(null);
  const [appVersion, setAppVersion] = useState<string | null>(null);
  // Seed from the cross-mount status snapshot so re-mounting on tab switch
  // paints the quick-control segs at their real values (still disabled until
  // statusReady) instead of flashing the fallbacks.
  const [proxy, setProxy] = useState<ProxyStatus | null>(() =>
    peekProxyStatus(),
  );
  /** false until status wave lands; details (nodes/subs) may still be loading. */
  const [statusReady, setStatusReady] = useState(false);
  const [detailsReady, setDetailsReady] = useState(false);
  const [error, setError] = useState<string | null>(null);
  /** Retry action for the `error` modal — e.g. re-running a TUN toggle after
   *  a permission prompt (setuid / UAC) so the user doesn't have to guess
   *  which switch to flip. Keyed to the exact message it was computed for:
   *  `error` is set from ~15 call sites and most don't know about this
   *  action, so keying (rather than clearing it at each of those sites)
   *  is what keeps a stale action from surviving onto an unrelated error. */
  const [errorAction, setErrorAction] = useState<{
    forMessage: string;
    label: string;
    onClick: () => void;
  } | null>(null);
  /** Core failure text to surface in the error modal (dead core, not running).
   *  Remembered dismissal keeps the same message from re-popping on every
   *  status poll / remount; a new failure text re-opens the modal. */
  const [dismissedCoreError, setDismissedCoreError] = useState<string | null>(
    null,
  );
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<GenerateConfigResult | null>(null);
  const [showPreview, setShowPreview] = useState(false);
  /** Config-preview modal: full-text search with jump-to-match + copy. */
  const [previewQuery, setPreviewQuery] = useState("");
  const [previewMatchIdx, setPreviewMatchIdx] = useState(0);
  const [previewCopied, setPreviewCopied] = useState(false);
  const previewPreRef = useRef<HTMLPreElement | null>(null);
  /** Bootstrap probe after enabling smart switch (does not lock other controls). */
  const [smartProbing, setSmartProbing] = useState(false);
  const smartGenRef = useRef(0);
  const [modeBusy, setModeBusy] = useState(false);
  const [latencyProbing, setLatencyProbing] = useState(false);
  const [envCopied, setEnvCopied] = useState(false);
  const [toast, setToast] = useState<string | null>(null);
  const [moreOpen, setMoreOpen] = useState(false);
  const [configMenuOpen, setConfigMenuOpen] = useState(false);
  const [coreMenuOpen, setCoreMenuOpen] = useState(false);
  const moreRef = useRef<HTMLDivElement>(null);
  /** Flyout close timer: leaving a sub row schedules a close; re-entering
   *  (row or popup) cancels it — covers the transit over sibling rows. */
  const subLeaveTimer = useRef<number | null>(null);
  function cancelSubmenuClose() {
    if (subLeaveTimer.current != null) {
      window.clearTimeout(subLeaveTimer.current);
      subLeaveTimer.current = null;
    }
  }
  function scheduleSubmenuClose() {
    cancelSubmenuClose();
    subLeaveTimer.current = window.setTimeout(() => {
      setConfigMenuOpen(false);
      setCoreMenuOpen(false);
    }, 280);
  }
  useEffect(() => cancelSubmenuClose, []);
  const [spark, setSpark] = useState<
    { up: number; down: number; conns: number }[]
  >([]);

  const pushSpark = useCallback((s: ProxyStatus | null) => {
    setSpark((prev) => {
      const next = [
        ...prev,
        {
          up: s?.upload_speed ?? 0,
          down: s?.download_speed ?? 0,
          conns: s?.connections ?? 0,
        },
      ];
      return next.length > 60 ? next.slice(next.length - 60) : next;
    });
  }, []);

  /** Display name for a core_type token (menu / version card / preview). */
function coreDisplayName(kind: string | null | undefined): string {
  return kind === "xray" ? "Xray" : kind === "mihomo" ? "mihomo" : "sing-box";
}

/** Full reload (actions after start/stop/etc). */
  const reload = useCallback(async () => {
    setError(null);
    try {
      // Kick both waves at once; commit status as soon as wave 1 resolves.
      const statusP = Promise.all([
        getSettings(),
        getProxyStatus().catch(() => null),
      ]);
      const detailP = Promise.all([
        listSubscriptions(),
        listAllNodes(),
        getCoreInfo("singbox").catch(() => null),
        getCoreInfo("xray").catch(() => null),
        getCoreInfo("mihomo").catch(() => null),
        getLanIp().catch(() => null),
      ]);

      const [settings, status] = await statusP;
      setPendingCore(settings.core_type ?? null);
      setSettingsPorts({
        mixed: settings.mixed_port,
        api: settings.api_port,
        extras: settings.extra_inbounds ?? [],
      });
      setCurrentNodeId(settings.current_node_id ?? null);
      setProxy(status);
      pushSpark(status);
      setStatusReady(true);

      const [subList, nodeList, coreSingbox, coreXray, coreMihomo, lan] =
        await detailP;
      setSubs(subList);
      setNodes(nodeList);
      setLanIp(lan ?? null);
      const cur =
        nodeList.find((n) => n.id === settings.current_node_id) ??
        nodeList[0] ??
        null;
      setCurrentNode(cur);
      // Version card shows the ACTIVE core's name + version (core_type reports
      // the actually running kind; while stopped it falls back to the setting).
      const activeKind = status?.core_type ?? settings.core_type ?? "singbox";
      const core =
        activeKind === "xray"
          ? coreXray
          : activeKind === "mihomo"
            ? coreMihomo
            : coreSingbox;
      if (core?.installed) {
        const ver = (core.version ?? "ok").replace(/^v/, "");
        setCoreVersion(`${core.name} ${ver}`.trim());
      } else {
        setCoreVersion(null);
      }
      setDetailsReady(true);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
      setStatusReady(true);
      setDetailsReady(true);
    }
  }, [pushSpark, t]);

  useEffect(() => {
    void reload();
  }, [reload]);

  useEffect(() => {
    void getVersion()
      .then(setAppVersion)
      .catch(() => undefined);
  }, []);

  // requestCaptureMode is defined by the hook below but the retry action
  // needs to call back into it — a ref breaks the definition-order cycle
  // without restructuring the hook wiring.
  const requestCaptureModeRef = useRef<((mode: CaptureMode) => void) | null>(
    null,
  );

  const onCaptureError = useCallback((msg: string) => {
    setError(msg);
    setErrorAction(
      isTunPermissionError(msg)
        ? {
            forMessage: msg,
            label: t("dashboard.tunReauthorize"),
            onClick: () => {
              setError(null);
              setErrorAction(null);
              requestCaptureModeRef.current?.("tun");
            },
          }
        : null,
    );
  }, [t]);

  // Hook only invokes this when the drain batch touched TUN (core restart).
  const onCaptureApplied = useCallback(() => {
    void reload();
  }, [reload]);

  const { captureMode, captureBusy, requestCaptureMode } = useCaptureModeSwitch(
    proxy,
    setProxy,
    onCaptureError,
    onCaptureApplied,
  );
  requestCaptureModeRef.current = requestCaptureMode;

  useVisibleInterval(() => {
    // Do not clobber optimistic capture UI while a switch is in flight.
    if (captureBusy) return;
    return getProxyStatus()
      .then((s) => {
        setProxy(s);
        pushSpark(s);
        setPendingCore(peekSettings()?.core_type ?? s.core_type ?? null);
        // Kernel auto-select (any core) and app-side smart picks persist the
        // current node in the backend; the status carries it — reflect a
        // change without waiting for a full reload. Only touch the display
        // node when it's in the loaded list (avoid flashing "disconnected"
        // mid-subscription-switch) and never clobber optimistic switch UI.
        const nid = s.current_node_id ?? null;
        if (
          !busy &&
          !customRuntime &&
          nid !== currentNodeId &&
          nid !== currentNode?.id
        ) {
          setCurrentNodeId(nid);
          const node = nodes.find((n) => n.id === nid);
          if (node) setCurrentNode(node);
        }
      })
      .catch(() => undefined);
  }, 1000);

  // Core switch transition: the setting flips instantly but the running core
  // only lands on the new binary after the debounced restart. While the two
  // disagree (and no custom profile is active — those legitimately run
  // sing-box), the status card shows a spinner instead of the stale old-core
  // facts. The 1s poll keeps both sides fresh from any switch entry point.
  const [pendingCore, setPendingCore] = useState<string | null>(
    () => peekSettings()?.core_type ?? null,
  );
  const coreSwitching =
    !!proxy?.running &&
    proxy?.runtime_source !== "singbox" &&
    pendingCore != null &&
    proxy?.core_type != null &&
    pendingCore !== proxy?.core_type;

  // Core switches restart asynchronously (500ms debounce + process restart),
  // and the switch commands return before that lands. The 1s poll above
  // observes the new core_type the moment the restart completes — react to
  // that edge with a full reload so the version card, memory readout and the
  // (core-filtered) node list all settle, no matter which entry point
  // (hero ⋯, top-bar ⋯, Settings) triggered the switch.
  const prevCoreTypeRef = useRef<string | null>(null);
  useEffect(() => {
    const kind = proxy?.core_type ?? null;
    if (kind == null) return;
    const first = prevCoreTypeRef.current === null;
    const changed = prevCoreTypeRef.current !== kind;
    prevCoreTypeRef.current = kind;
    if (!first && changed) void reload();
  }, [proxy?.core_type, reload]);

  useEffect(() => {
    if (!moreOpen) return;
    function onDoc(e: MouseEvent) {
      if (moreRef.current && !moreRef.current.contains(e.target as Node)) {
        setMoreOpen(false);
        setConfigMenuOpen(false);
      }
    }
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [moreOpen]);

  async function onStart() {
    setBusy(true);
    setError(null);
    try {
      const s = await startProxy(false);
      setProxy(s);
      await reload();
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setBusy(false);
    }
  }

  function resolveAutoSelect(p: ProxyStatus | null): AutoSelectMode {
    const raw = (p?.auto_select ?? (p?.smart_switch ? "smart" : "off")) as string;
    if (raw === "smart" || raw === "kernel") return raw;
    return "off";
  }

  async function onSetAutoSelect(mode: AutoSelectMode) {
    if (mode === autoSelectMode) return;
    setError(null);
    const prev = autoSelectMode;

    // Leaving smart: cancel any in-flight bootstrap probe.
    if (mode !== "smart") {
      smartGenRef.current += 1;
      setSmartProbing(false);
    }

    setProxy((p) =>
      p
        ? {
            ...p,
            auto_select: mode,
            smart_switch: mode === "smart",
          }
        : p,
    );

    const gen = ++smartGenRef.current;
    if (mode === "smart") setSmartProbing(true);

    try {
      await updateSettings({ autoSelect: mode });
      if (gen !== smartGenRef.current) return;

      if (mode === "smart") {
        try {
          const r = await smartSwitchNow();
          if (gen !== smartGenRef.current) return;
          if (r.message === "core not running") {
            setError(t("dashboard.smartSwitchNeedCore"));
          } else if (r.message === "all probes failed") {
            setError(t("dashboard.smartSwitchProbeFail"));
          } else if (r.message === "no nodes") {
            setError(t("dashboard.smartSwitchNoNodes"));
          } else if (r.message === "clash api unavailable") {
            setError(t("dashboard.smartSwitchProbeFail"));
          }
        } catch (probeErr) {
          if (gen !== smartGenRef.current) return;
          setError(
            typeof probeErr === "string" ? probeErr : String(probeErr),
          );
        }
      }

      if (gen !== smartGenRef.current) return;
      await reload();
      const s = await getProxyStatus().catch(() => null);
      if (s) setProxy(s);
    } catch (e) {
      if (gen === smartGenRef.current) {
        setError(typeof e === "string" ? e : String(e));
        setProxy((p) =>
          p
            ? {
                ...p,
                auto_select: prev,
                smart_switch: prev === "smart",
              }
            : p,
        );
      }
    } finally {
      if (gen === smartGenRef.current) setSmartProbing(false);
    }
  }

  async function onSetMode(mode: OutboundMode) {
    if ((proxy?.outbound_mode ?? "rule") === mode || modeBusy) return;
    setModeBusy(true);
    setError(null);
    try {
      const s = await setOutboundMode(mode);
      setProxy(s);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
      const s = await getProxyStatus().catch(() => null);
      if (s) setProxy(s);
    } finally {
      setModeBusy(false);
    }
  }

  async function onStop() {
    setBusy(true);
    setError(null);
    try {
      const s = await stopProxy();
      setProxy(s);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function onRestart() {
    setBusy(true);
    setError(null);
    setMoreOpen(false);
    try {
      const s = await restartProxy();
      setProxy(s);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setBusy(false);
    }
  }

  const customProfiles = useMemo(
    () => subs.filter((s) => s.source_kind === "singbox"),
    [subs],
  );
  const selectedCustomId =
    proxy?.runtime_source === "singbox" ? (proxy.runtime_profile_id ?? null) : null;
  // Custom sing-box profiles cannot run under the Xray/mihomo cores (they
  // always use the sing-box binary); grey them out while a foreign core is
  // active. The "generated" option stays live — it generates for the active
  // core. While a custom profile IS running, core_type truthfully reports
  // singbox, so switching between profiles / back to generated keeps working.
  const foreignCore =
    proxy?.core_type === "xray" || proxy?.core_type === "mihomo";

  async function onPickRuntime(source: string) {
    if (busy) return;
    setBusy(true);
    setError(null);
    setMoreOpen(false);
    setConfigMenuOpen(false);
    try {
      await setRuntimeSource(source);
      const s = await getProxyStatus();
      setProxy(s);
      await reload();
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setBusy(false);
    }
  }

  /** Hero ⋯ → 指定内核: switch the active core (restarts a running core). */
  async function onSwitchCore(kind: CoreKind) {
    if (busy || (proxy?.core_type ?? "singbox") === kind) return;
    setBusy(true);
    setError(null);
    setMoreOpen(false);
    setCoreMenuOpen(false);
    try {
      await setCoreType(kind);
      const s = await getProxyStatus();
      setProxy(s);
      await reload();
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function onPreview() {
    setBusy(true);
    setError(null);
    setMoreOpen(false);
    setPreviewQuery("");
    setPreviewMatchIdx(0);
    setPreviewCopied(false);
    try {
      if (proxy?.runtime_source === "singbox" && proxy.runtime_profile_id) {
        const detail = await getSubscription(proxy.runtime_profile_id);
        setResult({
          path: proxy.config_path ?? "",
          selected_tag: "",
          outbound_count: 0,
          mixed_port: proxy.mixed_port,
          api_port: proxy.api_port,
          preview: detail.content ?? "",
        });
      } else {
        const r = await previewSingboxConfig();
        setResult(r);
      }
      setShowPreview(true);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setBusy(false);
    }
  }

  function closePreview() {
    setShowPreview(false);
    // Release the preview string + its split-lines array — generated configs
    // reach hundreds of KB and the dashboard is the always-mounted home page.
    setResult(null);
  }

  async function onProbeLatency() {
    if (!currentNode || latencyProbing) return;
    setLatencyProbing(true);
    setError(null);
    try {
      const batch = await testNodesLatency([currentNode.id], 3000);
      const r = batch.results.find((r) => r.id === currentNode.id);
      if (r) {
        setCurrentNode((n) =>
          n ? { ...n, latency_ms: r.latency_ms ?? null } : n,
        );
      }
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setLatencyProbing(false);
    }
  }

  const running = proxy?.running ?? false;
  const stateLabel = proxy?.core_state ?? "stopped";
  const outboundMode = (proxy?.outbound_mode ?? "rule") as OutboundMode;
  // Smart bootstrap probe must not lock routing / sys proxy / TUN.
  // captureBusy must NOT freeze other controls (optimistic capture runs long).
  const controlsBusy = busy || modeBusy;
  const autoSelectMode = resolveAutoSelect(proxy);
  const nodeCount = nodes.length;
  const subCount = subs.length;
  // Allow start once we know a node id, even if full list is still loading.
  const customRuntime = proxy?.runtime_source === "singbox";
  const canStart =
    customRuntime || nodeCount > 0 || (!!currentNodeId && statusReady);
  const mixedPort = proxy?.mixed_port ?? settingsPorts.mixed;
  // Multi-listen ports only matter for the generated config (custom mode
  // launches the user's own file; its inbound shows on the config card).
  const extraInbounds = customRuntime ? [] : settingsPorts.extras;
  const extraListenTooltip = extraInbounds
    .map((e) => `${e.kind} ${e.allow_lan ? "0.0.0.0" : "127.0.0.1"}:${e.port}`)
    .join("  ·  ");

  const switching =
    stateLabel === "starting" || stateLabel === "stopping" || busy;
  const isError = stateLabel === "error" || (!!proxy?.error && !running);

  const stateUpper = running
    ? "RUNNING"
    : switching
      ? stateLabel === "stopping"
        ? "STOPPING"
        : "STARTING"
      : isError
        ? "ERROR"
        : "STOPPED";

  const dotClass = running
    ? "on"
    : switching
      ? "busy"
      : isError
        ? "off"
        : "off";

  const orbitState = running
    ? "live"
    : switching
      ? "switching"
      : isError
        ? "error"
        : "stopped";

  const heroTitle = !detailsReady && running
    ? null // skeleton
    : customRuntime
      ? t("dashboard.customMode", {
          name: proxy?.runtime_profile_name || t("config.singbox"),
        })
      : running
        ? currentNode?.name ?? t("dashboard.disconnected")
        : isError
          ? t("dashboard.errorTitle")
          : t("dashboard.disconnected");

  const heroSub = !detailsReady && running
    ? null
    : customRuntime
      ? t("config.singboxReadonly")
      : running
        ? [currentNode?.protocol?.toUpperCase(), fmtLatency(currentNode?.latency_ms)]
            .filter(Boolean)
            .join(" · ")
        : t("dashboard.desc");

  // Long node names shrink to one line instead of wrapping the hero.
  const heroTitleRef = useSingleLineFit<HTMLHeadingElement>(heroTitle ?? "");

  /** Best / avg among nodes that have a successful latency sample. */
  const latencyStats = useMemo(() => {
    const samples: number[] = nodes
      .map((n) => n.latency_ms)
      .filter((ms): ms is number => ms != null && ms >= 0);
    if (samples.length === 0) {
      return { best: null as number | null, avg: null as number | null, n: 0 };
    }
    const best = Math.min(...samples);
    const avg = Math.round(samples.reduce((a, b) => a + b, 0) / samples.length);
    return { best, avg, n: samples.length };
  }, [nodes]);

  async function onCopyEnv() {
    const proxyUrl = `http://127.0.0.1:${mixedPort}`;
    const isWindows = /Windows/i.test(navigator.userAgent);
    const text = isWindows
      ? `$env:ALL_PROXY = "${proxyUrl}"`
      : `export all_proxy=${proxyUrl}`;
    try {
      await navigator.clipboard.writeText(text);
      setEnvCopied(true);
      setMoreOpen(false);
      setToast(t("dashboard.envCopied"));
      window.setTimeout(() => setEnvCopied(false), 1500);
      window.setTimeout(() => setToast(null), 1500);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  async function onCopyPreview() {
    if (!result) return;
    try {
      await navigator.clipboard.writeText(result.preview);
      setPreviewCopied(true);
      setToast(t("common.copied"));
      window.setTimeout(() => setPreviewCopied(false), 1500);
      window.setTimeout(() => setToast(null), 1500);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  /** Config preview split once per fetch; search runs over these lines. */
  const previewLines = useMemo(
    () => (result?.preview ?? "").split("\n"),
    [result?.preview],
  );
  const previewQueryTrimmed = previewQuery.trim();
  /** Line indices containing the query (null = no query, plain-text fast path).
   *  `set` gives O(1) membership while rendering every line. */
  const previewMatches = useMemo(() => {
    const q = previewQueryTrimmed.toLowerCase();
    if (!q) return null;
    const indices: number[] = [];
    previewLines.forEach((text, n) => {
      if (text.toLowerCase().includes(q)) indices.push(n);
    });
    return { indices, set: new Set(indices) };
  }, [previewLines, previewQueryTrimmed]);

  const previewCurIdx =
    previewMatches && previewMatches.indices.length > 0
      ? Math.min(previewMatchIdx, previewMatches.indices.length - 1)
      : 0;

  // Jump-to-match: scroll the full-text preview to the current hit. Instant
  // (no smooth) — generated configs span thousands of pixels and the WebView
  // animates that poorly.
  useEffect(() => {
    if (!showPreview || !previewMatches || previewMatches.indices.length === 0) {
      return;
    }
    const n = previewMatches.indices[previewCurIdx];
    const el = previewPreRef.current?.querySelector<HTMLElement>(
      `[data-n="${n}"]`,
    );
    el?.scrollIntoView({ block: "center" });
  }, [showPreview, previewMatches, previewCurIdx]);

  const enabledSubs = useMemo(() => subs.filter((s) => s.enabled), [subs]);

  const selectedCustom = useMemo(
    () =>
      customRuntime
        ? (subs.find((s) => s.id === proxy?.runtime_profile_id) ?? null)
        : null,
    [customRuntime, proxy?.runtime_profile_id, subs],
  );

  const activeSub = enabledSubs[0] ?? subs[0] ?? null;

  const visibleSubs = useMemo(
    () => (enabledSubs.length > 0 ? enabledSubs : activeSub ? [activeSub] : []),
    [activeSub, enabledSubs],
  );

  const activeSubNames = useMemo(
    () => visibleSubs.map((sub) => sub.name).join(" · "),
    [visibleSubs],
  );

  const subQuota = useMemo(() => {
    const empty = {
      used: 0,
      total: null as number | null,
      remaining: null as number | null,
      ratio: null as number | null,
      label: "—",
    };
    const parts = visibleSubs
      .map((sub) => quotaParts(sub.traffic))
      .filter((p): p is NonNullable<typeof p> => p != null);
    if (parts.length === 0) return empty;

    const withTotal = parts.filter((p) => p.total != null);
    if (withTotal.length > 0) {
      const used = withTotal.reduce((sum, p) => sum + p.used, 0);
      const total = withTotal.reduce((sum, p) => sum + (p.total ?? 0), 0);
      const ratio = total > 0 ? Math.min(1, used / total) : 0;
      return {
        used,
        total,
        remaining: Math.max(0, total - used),
        ratio,
        label: `${fmtBytes(used)} / ${fmtBytes(total)}`,
      };
    }

    const withRemaining = parts.filter((p) => p.remaining != null);
    if (withRemaining.length > 0) {
      const remaining = withRemaining.reduce(
        (sum, p) => sum + (p.remaining ?? 0),
        0,
      );
      return {
        used: 0,
        total: null,
        remaining,
        ratio: null,
        label: t("common.remaining", { n: fmtBytes(remaining) }),
      };
    }
    return empty;
  }, [t, visibleSubs]);

  const quotaPct =
    subQuota.ratio != null ? Math.round(subQuota.ratio * 100) : null;
  const quotaLevel =
    subQuota.ratio == null
      ? ""
      : subQuota.ratio >= 0.9
        ? "critical"
        : subQuota.ratio >= 0.7
          ? "warn"
          : "ok";

  const currentLatency = currentNode?.latency_ms;
  /** Core failure while stopped — surfaced as a modal unless dismissed. */
  const coreErrorText = proxy?.error && !running ? proxy.error : null;

  return (
    <div className="page dashboard-page">
      {toast && <div className="toast">{toast}</div>}
      {error && (
        <ErrorModal
          message={error}
          onClose={() => {
            setError(null);
            setErrorAction(null);
          }}
          action={
            errorAction?.forMessage === error ? errorAction : undefined
          }
        />
      )}
      {coreErrorText != null && coreErrorText !== dismissedCoreError && (
        <ErrorModal
          message={coreErrorText}
          onClose={() => setDismissedCoreError(coreErrorText)}
        />
      )}

      {/* —— Hero: earth globe (starfield backdrop) or orbit visual + status —— */}
      <section className={`dash-hero is-${orbitState}`}>
        {heroIsGlobe ? (
          <EarthGlobeLazy />
        ) : (
          <HeroVisual
            state={orbitState}
            spinning={running || switching}
            switching={switching}
          />
        )}

        <div className="dash-hero-copy">
          <div className="dash-kicker mono">
            <span className={`status-dot ${dotClass}`} />
            {stateUpper}
            <span className="dash-kicker-sep">·</span>
            SATELITE {appVersion ?? "—"}
          </div>

          <h1 className="dash-hero-title" ref={heroTitleRef} title={heroTitle ?? undefined}>
            {heroTitle == null ? (
              <span className="skel skel-inline skel-w-40" aria-hidden />
            ) : (
              heroTitle
            )}
          </h1>
          <p className="dash-hero-desc">
            {heroSub == null ? (
              <span className="skel skel-inline skel-w-30" aria-hidden />
            ) : (
              heroSub
            )}
          </p>

          <div className="dash-hero-actions">
            {!running ? (
              <button
                type="button"
                className="btn-pill"
                disabled={busy || !canStart || switching || !statusReady}
                onClick={() => void onStart()}
              >
                {busy || stateLabel === "starting"
                  ? t("dashboard.starting")
                  : isError
                    ? t("dashboard.retry")
                    : t("dashboard.start")}
              </button>
            ) : (
              <button
                type="button"
                className="btn-pill danger"
                disabled={busy || switching}
                onClick={() => void onStop()}
              >
                {t("dashboard.stop")}
              </button>
            )}

            <button
              type="button"
              className="btn-pill secondary"
              disabled={!canStart}
              onClick={() => onGoNodes?.()}
            >
              {t("dashboard.switchNode")}
            </button>

            <div className="dash-more" ref={moreRef}>
              <button
                type="button"
                className="btn-pill ghost dash-more-btn"
                aria-expanded={moreOpen}
                aria-haspopup="menu"
                onClick={() => {
                  setMoreOpen((v) => {
                    // Fresh open: submenus start closed (no stale hover state).
                    if (!v) {
                      setConfigMenuOpen(false);
                      setCoreMenuOpen(false);
                    }
                    return !v;
                  });
                }}
              >
                ···
              </button>
              {moreOpen && (
                <div className="dash-more-menu card glass" role="menu">
                  <button
                    type="button"
                    role="menuitem"
                    disabled={busy || !running}
                    onClick={() => void onRestart()}
                  >
                    {busy && running ? (
                      <>
                        <span
                          className="lat-spinner ui-mode-restart-spinner"
                          aria-hidden
                        />{" "}
                        {t("dashboard.restart")}
                      </>
                    ) : (
                      t("dashboard.restart")
                    )}
                  </button>
                  <button
                    type="button"
                    role="menuitem"
                    onClick={() => void onCopyEnv()}
                  >
                    {envCopied
                      ? t("dashboard.envCopied")
                      : t("dashboard.copyEnv")}
                  </button>
                  <button
                    type="button"
                    role="menuitem"
                    disabled={busy || !canStart}
                    onClick={() => void onPreview()}
                  >
                    {t("common.preview")}
                  </button>
                  {/* Submenus open on hover (click still works for touch) and
                      are mutually exclusive — hovering one closes the other. */}
                  <div
                    className="dash-more-sub"
                    onPointerEnter={() => {
                      cancelSubmenuClose();
                      setConfigMenuOpen(true);
                      setCoreMenuOpen(false);
                    }}
                    onPointerLeave={scheduleSubmenuClose}
                  >
                    <button
                      type="button"
                      role="menuitem"
                      aria-haspopup="menu"
                      aria-expanded={configMenuOpen}
                      disabled={busy}
                      onClick={() => {
                        setConfigMenuOpen(true);
                        setCoreMenuOpen(false);
                      }}
                    >
                      <span>{t("dashboard.pickConfig")}</span>
                      <span className="dash-more-caret" aria-hidden>
                        ›
                      </span>
                    </button>
                    {configMenuOpen && (
                      <div className="dash-more-submenu card glass" role="menu">
                        <button
                          type="button"
                          role="menuitemradio"
                          aria-checked={!selectedCustomId}
                          className={!selectedCustomId ? "is-selected" : ""}
                          disabled={busy}
                          onClick={() => void onPickRuntime("generated")}
                        >
                          <span className="dash-more-check" aria-hidden>
                            {!selectedCustomId ? "●" : "○"}
                          </span>
                          {t("dashboard.pickConfigDefault")}
                        </button>
                        {customProfiles.map((profile) => {
                          const selected = selectedCustomId === profile.id;
                          return (
                            <button
                              key={profile.id}
                              type="button"
                              role="menuitemradio"
                              aria-checked={selected}
                              aria-disabled={foreignCore || undefined}
                              className={`${
                                selected ? "is-selected" : ""
                              }${foreignCore ? " is-disabled" : ""}`}
                              disabled={busy || foreignCore}
                              title={
                                foreignCore
                                  ? t("dashboard.customProfileNeedsSingbox")
                                  : profile.name
                              }
                              onClick={() =>
                                void onPickRuntime(`singbox:${profile.id}`)
                              }
                            >
                              <span className="dash-more-check" aria-hidden>
                                {selected ? "●" : "○"}
                              </span>
                              {profile.name}
                            </button>
                          );
                        })}
                      </div>
                    )}
                  </div>
                  <div
                    className="dash-more-sub"
                    onPointerEnter={() => {
                      cancelSubmenuClose();
                      setCoreMenuOpen(true);
                      setConfigMenuOpen(false);
                    }}
                    onPointerLeave={scheduleSubmenuClose}
                  >
                    <button
                      type="button"
                      role="menuitem"
                      aria-haspopup="menu"
                      aria-expanded={coreMenuOpen}
                      disabled={busy}
                      onClick={() => {
                        setCoreMenuOpen(true);
                        setConfigMenuOpen(false);
                      }}
                    >
                      <span>{t("dashboard.pickCore")}</span>
                      <span className="dash-more-caret" aria-hidden>
                        ›
                      </span>
                    </button>
                    {coreMenuOpen && (
                      <div className="dash-more-submenu card glass" role="menu">
                        {(["singbox", "xray", "mihomo"] as const).map((kind) => {
                          const selected =
                            (proxy?.core_type ?? "singbox") === kind;
                          return (
                            <button
                              key={kind}
                              type="button"
                              role="menuitemradio"
                              aria-checked={selected}
                              className={selected ? "is-selected" : ""}
                              disabled={busy}
                              onClick={() => void onSwitchCore(kind)}
                            >
                              <span className="dash-more-check" aria-hidden>
                                {selected ? "●" : "○"}
                              </span>
                              {coreDisplayName(kind)}
                            </button>
                          );
                        })}
                      </div>
                    )}
                  </div>
                </div>
              )}
            </div>
          </div>
        </div>

        {/* Right rail: light controls, no card chrome */}
        <aside className="dash-side-rail" aria-label="Quick controls">
          <div className="dash-rail-title mono">{t("dashboard.quickControls")}</div>
          <div className="dash-inline-row dash-rail-block">
            <span className="dash-inline-label">{t("dashboard.routing")}</span>
            <GlassSeg
              value={outboundMode}
              ready={statusReady}
              ariaLabel={t("dashboard.routing")}
              disabled={controlsBusy || !statusReady || customRuntime}
              onChange={(v) => void onSetMode(v as OutboundMode)}
              options={[
                { value: "rule", label: t("dashboard.modeRule") },
                { value: "global", label: t("dashboard.modeGlobal") },
                { value: "direct", label: t("dashboard.modeDirect") },
              ]}
            />
          </div>
          <div className="dash-inline-row dash-auto-select">
            <span
              className={`dash-inline-label${smartProbing ? " dash-smart-probing" : ""}`}
              title={
                smartProbing
                  ? t("dashboard.smartSwitchProbing")
                  : t("dashboard.autoSelectDesc")
              }
            >
              {smartProbing ? (
                <>
                  <span className="lat-spinner dash-smart-spinner" aria-hidden />
                  <span>{t("dashboard.smartSwitchProbing")}</span>
                </>
              ) : (
                t("dashboard.autoSelect")
              )}
            </span>
            <GlassSeg
              value={autoSelectMode}
              ready={statusReady}
              ariaLabel={t("dashboard.autoSelect")}
              disabled={modeBusy || !statusReady || customRuntime}
              disabledValues={
                new Set(
                  [
                    smartProbing ? "smart" : null,
                    proxy?.core_type === "xray" ? "smart" : null,
                    nodeCount === 0 &&
                    autoSelectMode === "off" &&
                    !smartProbing
                      ? "kernel"
                      : null,
                    nodeCount === 0 &&
                    autoSelectMode === "off" &&
                    !smartProbing
                      ? "smart"
                      : null,
                  ].filter((v): v is string => v != null),
                )
              }
              titles={{
                kernel: t("dashboard.autoSelectKernelHint"),
                smart:
                  proxy?.core_type === "xray"
                    ? t("dashboard.smartSwitchNeedsSingbox")
                    : t("dashboard.smartSwitchDesc"),
                off: t("dashboard.autoSelectDesc"),
              }}
              onChange={(v) => void onSetAutoSelect(v as AutoSelectMode)}
              options={[
                { value: "off", label: t("dashboard.autoSelectOff") },
                { value: "kernel", label: t("dashboard.autoSelectKernel") },
                { value: "smart", label: t("dashboard.autoSelectSmart") },
              ]}
            />
          </div>
          <div className="dash-inline-row dash-auto-select dash-capture">
            <span
              className={`dash-inline-label${captureBusy ? " dash-smart-probing" : ""}`}
              title={
                captureBusy
                  ? t("dashboard.captureSwitching")
                  : t("dashboard.captureDesc")
              }
            >
              {captureBusy ? (
                <>
                  <span className="lat-spinner dash-smart-spinner" aria-hidden />
                  <span>{t("dashboard.captureSwitching")}</span>
                </>
              ) : (
                t("dashboard.capture")
              )}
            </span>
            <GlassSeg
              value={captureMode}
              ready={statusReady}
              ariaLabel={t("dashboard.capture")}
              disabled={!statusReady}
              disabledValues={
                new Set(
                  [
                    customRuntime || (nodeCount === 0 && captureMode !== "tun")
                      ? "tun"
                      : null,
                    customRuntime && !proxy?.custom_inbound_port
                      ? "system"
                      : null,
                  ].filter((v): v is string => v != null),
                )
              }
              titles={{
                tun: t("dashboard.captureTunHint"),
                system: t("dashboard.captureSystemHint"),
                off: t("dashboard.captureDesc"),
              }}
              onChange={(v) => {
                setError(null);
                requestCaptureMode(v as "off" | "system" | "tun");
              }}
              options={[
                { value: "off", label: t("dashboard.captureOff") },
                { value: "system", label: t("dashboard.captureSystem") },
                { value: "tun", label: t("dashboard.captureTun") },
              ]}
            />
          </div>
        </aside>
      </section>

      {subCount === 0 && (
        <div className="dashboard-setup card glass">
          <p className="dashboard-setup-hint muted">
            {t("dashboard.noProfileHint")}
          </p>
          <button
            type="button"
            className="btn-pill"
            onClick={() => onGoProfiles?.()}
          >
            {t("dashboard.goAddProfile")}
          </button>
        </div>
      )}

      {/* —— 6 cards: core / spark / traffic+conns · quality / sub / system —— */}
      <section className="instrument-grid instrument-grid-6" aria-label="Telemetry">
        <article className="instrument accent-green">
          <header className="instrument-head">
            {/* Label carries the active core's name (core_type falls back to
                the setting while stopped, so this is correct either way). */}
            <span className="instrument-label">
              {t("dashboard.cardCore")} · {coreDisplayName(proxy?.core_type)}
            </span>
          </header>
          <div
            className={`instrument-value readout${
              running
                ? ""
                : switching
                  ? " state-busy"
                  : isError
                    ? " state-error"
                    : " state-off"
            }`}
          >
            {running
              ? fmtUptime(proxy?.core_started_at)
              : switching
                ? stateLabel === "stopping"
                  ? t("dashboard.coreStopping")
                  : t("dashboard.coreStarting")
                : isError
                  ? t("dashboard.coreError")
                  : t("dashboard.coreStopped")}
          </div>
          <div className="instrument-kv mono">
            <div>
              <span className="kv-k">{t("dashboard.version")}</span>
              <span className="kv-v">
                {coreSwitching ? (
                  <>
                    <span
                      className="lat-spinner ui-mode-restart-spinner"
                      aria-hidden
                    />{" "}
                    {t("dashboard.coreSwitching")}
                  </>
                ) : (
                  <>
                    {coreVersion ?? "—"}
                    {proxy?.core_elevated && (
                      <span
                        className="kv-v-badge is-danger"
                        title={t("dashboard.coreElevatedHint")}
                      >
                        {t("dashboard.coreElevated")}
                      </span>
                    )}
                  </>
                )}
              </span>
            </div>
            <div>
              <span className="kv-k">{t("dashboard.memory")}</span>
              <span className="kv-v">
                {coreSwitching ? (
                  <>
                    <span
                      className="lat-spinner ui-mode-restart-spinner"
                      aria-hidden
                    />{" "}
                    {t("dashboard.coreSwitching")}
                  </>
                ) : running && proxy?.core_memory_bytes != null ? (
                  fmtBytes(proxy.core_memory_bytes)
                ) : (
                  "—"
                )}
              </span>
            </div>
          </div>
        </article>

        <SimpleTrafficSpark
          samples={spark}
          up={proxy?.upload_speed ?? 0}
          down={proxy?.download_speed ?? 0}
          conns={proxy?.connections ?? 0}
          running={running}
          label={t("simple.spark")}
          idleLabel={t("simple.sparkIdle")}
          idleConnsLabel={t("simple.sparkIdleConns")}
          connsLabel={t("simple.sparkConns", { n: proxy?.connections ?? 0 })}
          onOpen={onGoTraffic}
        />

        <article
          className="instrument accent-blue instrument-click"
          role="button"
          tabIndex={0}
          onClick={() => onGoTraffic?.()}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") onGoTraffic?.();
          }}
        >
          <header className="instrument-head">
            <span className="instrument-label">
              {t("dashboard.cardTrafficStats")}
            </span>
          </header>
          <div className="instrument-traffic-cols">
            <div className="instrument-traffic-col">
              <div className="instrument-traffic-col-label">
                {t("dashboard.trafficLive")}
              </div>
              <div className="instrument-traffic">
                <div>
                  <span className="tr-dir down">↓</span>{" "}
                  {fmtSpeed(proxy?.download_speed ?? 0)}
                </div>
                <div>
                  <span className="tr-dir up">↑</span>{" "}
                  {fmtSpeed(proxy?.upload_speed ?? 0)}
                </div>
              </div>
            </div>
            <div className="instrument-traffic-col">
              <div className="instrument-traffic-col-label">
                {t("dashboard.trafficTotal")}
              </div>
              <div className="instrument-traffic">
                <div>
                  <span className="tr-sigma down">Σ</span>
                  <span className="tr-dir down">↓</span>{" "}
                  {fmtBytes(proxy?.download_total ?? 0)}
                </div>
                <div>
                  <span className="tr-sigma up">Σ</span>
                  <span className="tr-dir up">↑</span>{" "}
                  {fmtBytes(proxy?.upload_total ?? 0)}
                </div>
              </div>
            </div>
          </div>
        </article>

        <article
          className="instrument accent-cyan instrument-click"
          role="button"
          tabIndex={0}
          title={t("dashboard.probeLatencyHint")}
          onClick={() => void onProbeLatency()}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              void onProbeLatency();
            }
          }}
        >
          <header className="instrument-head">
            <span className="instrument-label">
              {t("dashboard.cardQuality")}
            </span>
          </header>
          <div
            className={`instrument-value readout mono ${latencyClass(currentLatency)}`}
          >
            {latencyProbing ? (
              <span className="lat-spinner" aria-label={t("dashboard.probeLatencyRunning")} />
            ) : (
              fmtLatency(currentLatency)
            )}
          </div>
          <div className="instrument-kv mono">
            <div>
              <span className="kv-k">{t("dashboard.latencyAvg")}</span>
              <span className="kv-v">{fmtLatency(latencyStats.avg)}</span>
            </div>
            <div>
              <span className="kv-k">{t("dashboard.latencyBest")}</span>
              <span className="kv-v">{fmtLatency(latencyStats.best)}</span>
            </div>
          </div>
        </article>

        <article
          className="instrument accent-green instrument-click"
          role="button"
          tabIndex={0}
          onClick={() => onGoProfiles?.()}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") onGoProfiles?.();
          }}
        >
          <header className="instrument-head">
            <span className="instrument-label">
              {customRuntime
                ? t("dashboard.cardCustom")
                : t("dashboard.cardSub")}
            </span>
          </header>
          <div
            className={`instrument-value readout instrument-subscription-names${
              customRuntime
                ? (selectedCustom?.name.length ?? 0) > 12
                  ? " wrap"
                  : ""
                : visibleSubs.length > 1 || (activeSubNames?.length ?? 0) > 12
                  ? " wrap"
                  : ""
            }`}
            title={
              customRuntime
                ? (selectedCustom?.name ?? undefined)
                : activeSubNames || undefined
            }
          >
            <span>
              {customRuntime
                ? selectedCustom?.name || t("dashboard.noSub")
                : activeSubNames || t("dashboard.noSub")}
            </span>
          </div>
          <div className="instrument-kv mono">
            {customRuntime ? (
              <>
                <div>
                  <span className="kv-k">{t("config.singbox")}</span>
                  <span className="kv-v">{t("config.singboxReadonly")}</span>
                </div>
                <div>
                  <span className="kv-k">IN</span>
                  <span className="kv-v">
                    {proxy?.custom_inbound_port
                      ? `:${proxy.custom_inbound_port}`
                      : "—"}
                  </span>
                </div>
              </>
            ) : (
              <>
                <div className="instrument-quota-row">
                  <span className="kv-k">{t("dashboard.quota")}</span>
                  {quotaPct != null ? (
                    <span
                      className={`instrument-quota-bar ${quotaLevel}`}
                      title={`${quotaPct}%`}
                      aria-label={`${quotaPct}%`}
                    >
                      <span
                        className="instrument-quota-fill"
                        style={{ width: `${quotaPct}%` }}
                      />
                    </span>
                  ) : null}
                  <span className="kv-v">{subQuota.label}</span>
                </div>
                <div>
                  <span className="kv-k">{t("dashboard.nodeCount")}</span>
                  <span className="kv-v">{nodeCount}</span>
                </div>
              </>
            )}
          </div>
        </article>

        <article
          className="instrument accent-cyan instrument-click"
          role="button"
          tabIndex={0}
          title={t("dashboard.copyEnvHint")}
          onClick={() => void onCopyEnv()}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              e.preventDefault();
              void onCopyEnv();
            }
          }}
        >
          <header className="instrument-head">
            <span className="instrument-label">
              {t("dashboard.cardSystem")}
            </span>
          </header>
          <div
            className={`instrument-value readout mono${extraInbounds.length ? " multi" : ""}`}
            title={extraInbounds.length ? extraListenTooltip : undefined}
          >
            :{mixedPort}
            {extraInbounds.map((e) => ` :${e.port}`)}
          </div>
          <div className="instrument-kv mono">
            <div>
              <span className="kv-k">{t("dashboard.lanIp")}</span>
              <span className="kv-v">{lanIp ?? "—"}</span>
            </div>
            <div>
              <span className="kv-k">ENV</span>
              <span className="kv-v">
                {envCopied
                  ? t("dashboard.envCopied")
                  : t("dashboard.copyEnvHint")}
              </span>
            </div>
          </div>
        </article>
      </section>

      {showPreview && result && (
        <div
          className="modal-backdrop"
        >
          <div
            className="modal preview-modal"
            role="dialog"
            aria-modal="true"
            aria-labelledby="preview-modal-title"
          >
            <header className="modal-header">
              <h2 id="preview-modal-title">
                {t("common.preview")}
                {/* Which core this document belongs to — Xray and sing-box
                    share several top-level keys, so make it explicit. */}
                <span
                  className="muted"
                  style={{ marginLeft: "0.5rem", fontSize: "0.8rem", fontWeight: 400 }}
                >
                  ·{" "}
                  {proxy?.runtime_source === "singbox"
                    ? "sing-box"
                    : coreDisplayName(proxy?.core_type ?? "singbox")}
                </span>
              </h2>
              <button
                type="button"
                className="icon-btn"
                onClick={closePreview}
                aria-label={t("common.close")}
              >
                ×
              </button>
            </header>
            <div className="modal-body preview-body">
              <div className="preview-toolbar">
                <input
                  className="search preview-search"
                  placeholder={t("dashboard.filterConfig")}
                  value={previewQuery}
                  onChange={(e) => {
                    setPreviewQuery(e.target.value);
                    setPreviewMatchIdx(0);
                  }}
                  onKeyDown={(e) => {
                    if (
                      e.key !== "Enter" ||
                      !previewMatches ||
                      previewMatches.indices.length === 0
                    ) {
                      return;
                    }
                    e.preventDefault();
                    const len = previewMatches.indices.length;
                    setPreviewMatchIdx((i) =>
                      e.shiftKey ? (i - 1 + len) % len : (i + 1) % len,
                    );
                  }}
                  title={t("dashboard.matchJumpHint")}
                  spellCheck={false}
                />
                {previewMatches && (
                  <span className="muted preview-match-count">
                    {previewMatches.indices.length === 0
                      ? t("dashboard.matchNone")
                      : t("dashboard.matchPos", {
                          cur: previewCurIdx + 1,
                          n: previewMatches.indices.length,
                        })}
                  </span>
                )}
                <GlassButton
                  onClick={() => void onCopyPreview()}
                  disabled={previewCopied}
                >
                  {previewCopied ? t("common.copied") : t("common.copy")}
                </GlassButton>
              </div>
              <pre className="preview-json" ref={previewPreRef}>
                {previewMatches
                  ? previewLines.map((text, n) => {
                      const isMatch = previewMatches.set.has(n);
                      const isCurrent =
                        previewMatches.indices.length > 0 &&
                        previewMatches.indices[previewCurIdx] === n;
                      return (
                        <span
                          className={`preview-line${isCurrent ? " current" : ""}`}
                          key={n}
                          data-n={n}
                        >
                          <span className="preview-line-no">{n + 1}</span>
                          <span className="preview-line-text">
                            {isMatch
                              ? highlightPreviewLine(
                                  text,
                                  previewQueryTrimmed,
                                )
                              : text}
                          </span>
                        </span>
                      );
                    })
                  : result.preview}
              </pre>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
