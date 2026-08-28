import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { listen } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import { openUrl } from "@tauri-apps/plugin-opener";
import {
  checkAppUpdate,
  checkCoreUpdate,
  diagnoseNetwork,
  downloadCore,
  getAppInstallPath,
  getCoreInfo,
  getProxyStatus,
  getSettings,
  regenerateApiSecret,
  restartProxy,
  setCoreType,
  updateSettings,
} from "../api";
import { GlassButton } from "../components/GlassButton";
import { SolidSelect } from "../components/SolidSelect";
import { GlassSeg } from "../components/GlassSeg";
import { GlassSwitchControl } from "../components/GlassSwitchControl";
import { ErrorModal } from "../components/ErrorModal";
import { TrayIconPicker } from "../components/TrayIconPicker";
import { DecryptReveal } from "../components/DecryptReveal";
import { CoreMark } from "../components/CoreMark";
import buymecoffeeUrl from "../assets/buymecoffee.png";
import { useI18n, type Locale, type MessageKey } from "../i18n";
import { ACCENTS, applyGlowToDom, isCustomHexAccent, resolveAccent } from "../theme/accents";
import { AccentColorPickerModal } from "../components/AccentColorPickerModal";
import { useTheme } from "../theme";
import type {
  AppSettings,
  CoreDownloadProgress,
  CoreInfo,
  CoreKind,
  DiagnosticIssue,
  ExtraInbound,
  HeroStyle,
  ThemeId,
} from "../types";
import { RulesPage } from "./RulesPage";
import { ChainPage } from "./ChainPage";
import { DnsPage } from "./DnsPage";
import { HostsPage } from "./HostsPage";

type SettingsTab = "app" | "ports" | "rules" | "chain" | "dns" | "hosts" | "core";

const CUSTOM_BLOCKED_TABS = new Set(["rules", "chain", "dns", "hosts"]);

/** Repository link shown in the bottom-right corner of the settings page. */
const PROJECT_URL = "https://github.com/zn0wii/satelite-proxy/";
/** Always-latest app release page, opened from the version tab. */
const RELEASES_URL = "https://github.com/zn0wii/satelite-proxy/releases/latest";

// Accent preset names are picked from the i18n catalog rather than
// AccentPreset.name (theme/accents.ts), which is display data only and not
// locale-aware.
const ACCENT_LABEL_KEY: Record<string, MessageKey> = {
  green: "accent.green",
  blue: "accent.blue",
  purple: "accent.purple",
  pink: "accent.pink",
  orange: "accent.orange",
  cyan: "accent.cyan",
};

/** Idle custom swatch: a rainbow ring hinting "pick any colour". Replaced by
 *  the stored custom hex (inline style) once a custom accent is active. */
const CUSTOM_DOT_RAINBOW =
  "conic-gradient(from 90deg, #f66, #fc6, #6c6, #6cd, #66c, #c6c, #f66)";

function fmtCoreBytes(value: number) {
  if (value < 1024 * 1024) return `${(value / 1024).toFixed(1)} KB`;
  return `${(value / (1024 * 1024)).toFixed(1)} MB`;
}

export function SettingsPage() {
  const { t, locale, setLocale } = useI18n();
  const { theme, setTheme, accent, setAccent, glow, setGlow, heroStyle, setHeroStyle, glassFrost, setGlassFrost } =
    useTheme();
  const [tab, setTab] = useState<SettingsTab>("app");
  const [settings, setSettings] = useState<AppSettings | null>(null);
  /** Sponsor QR panel (decrypt-reveal over the image). */
  const [sponsorOpen, setSponsorOpen] = useState(false);
  const [sponsorSession, setSponsorSession] = useState("0000");
  /** Custom accent color picker (the extra swatch after the presets). */
  const [accentPickerOpen, setAccentPickerOpen] = useState(false);
  const [glowPickerOpen, setGlowPickerOpen] = useState(false);
  /** Anchor + viewport position for the sponsor popup (portaled to <body>,
   * fixed above the trigger button so opening it never reflows the card). */
  const sponsorBtnRef = useRef<HTMLButtonElement | null>(null);
  const [sponsorPos, setSponsorPos] = useState<{ top: number; right: number } | null>(
    null,
  );

  // Click outside the panel/link dismisses it. Ignore clicks inside either
  // node: the link is portaled to <body>, so React stopPropagation does not
  // reliably reach this native document listener.
  useEffect(() => {
    if (!sponsorOpen) return;
    const close = (e: MouseEvent) => {
      const node = e.target;
      if (
        node instanceof Element &&
        node.closest(".sponsor-panel, .sponsor-link")
      ) {
        return;
      }
      setSponsorOpen(false);
    };
    document.addEventListener("click", close);
    return () => document.removeEventListener("click", close);
  }, [sponsorOpen]);
  const [mixed, setMixed] = useState("2080");
  /** Main mixed inbound listens on 0.0.0.0 (LAN) instead of 127.0.0.1. */
  const [allowLan, setAllowLan] = useState(false);
  const [api, setApi] = useState("19090");
  /** Gate for the Clash API secret — off by default (no unexplained key for
   * first-run users); the API stays reachable on 127.0.0.1 either way. */
  const [apiSecretEnabled, setApiSecretEnabled] = useState(false);
  const [probe, setProbe] = useState("");
  const [tunStack, setTunStack] = useState("mixed");
  /** IPv6 address on the TUN interface. Off by default — most nodes have no
   * v6 egress and a dual-stack tun makes Chrome prefer AAAA/v6, black-holing
   * every connection. */
  const [tunIpv6, setTunIpv6] = useState(false);
  /** Reject sniffed QUIC (UDP/443) so browsers fall back to TCP. */
  const [blockQuic, setBlockQuic] = useState(false);
  /** Bypass localhost and LAN segments with built-in direct rules. */
  const [bypassLan, setBypassLan] = useState(true);
  /** Extra inbound drafts — applied on card save (needs core restart). */
  const [extra, setExtra] = useState<ExtraInbound[]>([]);
  // Extra-inbound editor modal (add / edit share one form).
  const [inboundOpen, setInboundOpen] = useState(false);
  const [inboundEditId, setInboundEditId] = useState<string | null>(null);
  const [inboundKind, setInboundKind] = useState<"mixed" | "http">("mixed");
  const [inboundPort, setInboundPort] = useState("");
  const [inboundLan, setInboundLan] = useState(false);
  const [inboundError, setInboundError] = useState<string | null>(null);
  const [menuInboundId, setMenuInboundId] = useState<string | null>(null);
  /** Copy-feedback flag for the read-only Clash API secret field. */
  const [secretCopied, setSecretCopied] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  /** Detection-only network diagnostics (e.g. system DNS bypassing TUN).
   * Re-checked whenever TUN transitions off → on; never auto-applied. */
  const [netDiagnostics, setNetDiagnostics] = useState<DiagnosticIssue[]>([]);

  /** Per-core status (sing-box + Xray + mihomo). */
  const [cores, setCores] = useState<Record<CoreKind, CoreInfo | null>>({
    singbox: null,
    xray: null,
    mihomo: null,
  });
  const [coreBusyKind, setCoreBusyKind] = useState<CoreKind | null>(null);
  const [coreCheckingKind, setCoreCheckingKind] = useState<CoreKind | null>(null);
  const [coreError, setCoreError] = useState<string | null>(null);
  const [coreProxyAvailable, setCoreProxyAvailable] = useState(false);
  const [coreProgress, setCoreProgress] =
    useState<CoreDownloadProgress | null>(null);

  // App's own version card: local version is instant (getVersion), the
  // latest GitHub tag needs a network check that routes via the proxy
  // when the kernel is running (same strategy as the core check).
  const [appVersion, setAppVersion] = useState<string | null>(null);
  const [appUpdate, setAppUpdate] = useState<{
    current_version: string;
    latest_version: string;
    update_available: boolean;
    cached: boolean;
    checked_at: number | null;
  } | null>(null);
  const [appChecking, setAppChecking] = useState(false);
  const [appError, setAppError] = useState<string | null>(null);
  /** Absolute path of the app's own executable, shown like the kernel path. */
  const [appPath, setAppPath] = useState<string | null>(null);

  const tabs = useMemo(
    () =>
      [
        {
          id: "app" as const,
          label: t("settings.tabApp"),
          hint: t("settings.hintApp"),
        },
        {
          id: "ports" as const,
          label: t("settings.tabPorts"),
          hint: t("settings.hintPorts"),
        },
        {
          id: "rules" as const,
          label: t("settings.tabRules"),
          hint: t("settings.hintRules"),
        },
        {
          id: "chain" as const,
          label: t("settings.tabChain"),
          hint: t("settings.hintChain"),
        },
        {
          id: "dns" as const,
          label: t("settings.tabDns"),
          hint: t("settings.hintDns"),
        },
        {
          id: "hosts" as const,
          label: t("settings.tabHosts"),
          hint: t("settings.hintHosts"),
        },
        {
          id: "core" as const,
          label: t("settings.tabCore"),
          hint: t("settings.hintCore"),
        },
      ] as const,
    [t],
  );

  const runCoreUpdateCheck = useCallback(
    async (kind: CoreKind, localVersion: string | null, reportError: boolean) => {
      setCoreCheckingKind(kind);
      if (reportError) setCoreError(null);
      try {
        const update = await checkCoreUpdate(kind, localVersion);
        setCores((prev) => {
          const info = prev[kind];
          if (!info) return prev;
          return {
            ...prev,
            [kind]: {
              ...info,
              latest_version: update.latest_version,
              update_available: update.update_available,
            },
          };
        });
      } catch (e) {
        if (reportError) {
          setCoreError(typeof e === "string" ? e : String(e));
        }
      } finally {
        setCoreCheckingKind(null);
      }
    },
    [],
  );

  const reloadCore = useCallback(async () => {
    setCoreError(null);
    try {
      const results = await Promise.all([
        getCoreInfo("singbox"),
        getCoreInfo("xray"),
        getCoreInfo("mihomo"),
      ]);
      const [singbox, xray, mihomo] = results;
      setCores({ singbox, xray, mihomo });
      void runCoreUpdateCheck("singbox", singbox.version ?? null, false);
      void runCoreUpdateCheck("xray", xray.version ?? null, false);
      void runCoreUpdateCheck("mihomo", mihomo.version ?? null, false);
    } catch (e) {
      setCoreError(typeof e === "string" ? e : String(e));
    }
  }, [runCoreUpdateCheck]);

  useEffect(() => {
    getSettings()
      .then((s) => {
        setSettings(s);
        setMixed(String(s.mixed_port));
        setAllowLan(!!s.allow_lan);
        setApi(String(s.api_port));
        setApiSecretEnabled(!!s.api_secret_enabled);
        setProbe(s.probe_url);
        setTunStack(s.tun_stack || "mixed");
        setTunIpv6(!!s.tun_ipv6_enabled);
        setBlockQuic(!!s.block_quic);
        setBypassLan(s.bypass_lan !== false);
        setExtra(s.extra_inbounds ?? []);
      })
      .catch((e) => setError(typeof e === "string" ? e : String(e)));
    void reloadCore();
  }, [reloadCore]);

  /** reportError: surface failures (manual click). force: bypass the 6h
   * cache and hit the network — manual checks always refresh, auto checks
   * on tab open reuse a fresh cached result. */
  const runAppUpdateCheck = useCallback(
    async (reportError: boolean, force = false) => {
      setAppChecking(true);
      if (reportError) setAppError(null);
      try {
        setAppUpdate(await checkAppUpdate(force));
      } catch (e) {
        if (reportError) {
          setAppError(typeof e === "string" ? e : String(e));
        }
      } finally {
        setAppChecking(false);
      }
    },
    [],
  );

  useEffect(() => {
    if (tab !== "core") return;
    void getVersion()
      .then(setAppVersion)
      .catch(() => setAppVersion(null));
    void getAppInstallPath()
      .then(setAppPath)
      .catch(() => setAppPath(null));
    void runAppUpdateCheck(false);
  }, [tab, runAppUpdateCheck]);

  useEffect(() => {
    if (tab !== "core") return;
    void getProxyStatus()
      .then((status) => setCoreProxyAvailable(status.running))
      .catch(() => setCoreProxyAvailable(false));
  }, [tab]);

  // Close the inbound-row ⋮ menu on outside pointer-down / Escape.
  useEffect(() => {
    if (!menuInboundId) return;
    function onDocPointerDown(e: PointerEvent) {
      const t = e.target as HTMLElement | null;
      if (t?.closest?.("[data-inbound-menu]")) return;
      setMenuInboundId(null);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setMenuInboundId(null);
    }
    document.addEventListener("pointerdown", onDocPointerDown, true);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", onDocPointerDown, true);
      document.removeEventListener("keydown", onKey);
    };
  }, [menuInboundId]);

  useEffect(() => {
    // Settings tabs remount pages often; if this unmounts before listen()
    // resolves, dispose immediately so the listener doesn't leak.
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<CoreDownloadProgress>("core-download-progress", (event) => {
      setCoreProgress(event.payload);
      setCoreProxyAvailable(event.payload.via_proxy);
    }).then((dispose) => {
      if (disposed) dispose();
      else unlisten = dispose;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  /** Latest auto-apply fn (called from the debounced effect and re-queued
   * from its own finally when the user edited mid-flight). */
  const autoApplyRef = useRef<() => Promise<void>>(async () => {});
  const applyingRef = useRef(false);
  /** Bumped by the debounced effect on every draft change; snapshotted at the
   * start of an apply so `finally` can tell "user edited again while this
   * attempt was in flight" apart from "this same attempt just failed and
   * `dirty` is still true because the failed call never landed in `settings`".
   * Without this, a persistently-failing restart (e.g. LAN bind failure)
   * retries itself forever with no backoff. */
  const applyGenerationRef = useRef(0);
  /** Previous tun_enabled value, to detect the off → on transition. */
  const prevTunEnabledRef = useRef<boolean | undefined>(undefined);

  // Re-run detection-only network diagnostics whenever TUN turns on. Never
  // fires on every settings refresh — only on the actual off → on edge —
  // since the check involves a couple of shell-outs on macOS.
  useEffect(() => {
    const wasOn = prevTunEnabledRef.current;
    const isOn = !!settings?.tun_enabled;
    prevTunEnabledRef.current = isOn;
    if (wasOn === undefined || wasOn === isOn || !isOn) {
      if (!isOn) setNetDiagnostics([]);
      return;
    }
    diagnoseNetwork()
      .then((r) => setNetDiagnostics(r.issues))
      .catch(() => setNetDiagnostics([]));
  }, [settings?.tun_enabled]);

  /** Auto-commit the ports tab: save every draft (ports / LAN / probe /
   * stack / listeners) and restart the core when it is running. Drafts that
   * are still invalid (mid-typing) are skipped until they become valid. */
  const autoApplyNetwork = useCallback(async () => {
    if (applyingRef.current || !settings) return;
    const dirty =
      String(settings.mixed_port) !== mixed.trim() ||
      !!settings.allow_lan !== allowLan ||
      String(settings.api_port) !== api.trim() ||
      !!settings.api_secret_enabled !== apiSecretEnabled ||
      (settings.probe_url ?? "") !== probe ||
      (settings.tun_stack || "mixed") !== tunStack ||
      !!settings.tun_ipv6_enabled !== tunIpv6 ||
      !!settings.block_quic !== blockQuic ||
      (settings.bypass_lan !== false) !== bypassLan ||
      !sameInbounds(settings.extra_inbounds ?? [], extra);
    if (!dirty) return;
    // Invalid drafts (mid-typing or left behind): surface why we can't apply
    // yet; the banner clears on the next successful auto-commit.
    const mixedPort = Number(mixed);
    const apiPort = Number(api);
    if (!Number.isFinite(mixedPort) || mixedPort < 1 || mixedPort > 65535) {
      setError(t("settings.invalidMixed"));
      return;
    }
    if (!Number.isFinite(apiPort) || apiPort < 1 || apiPort > 65535) {
      setError(t("settings.invalidApi"));
      return;
    }
    const seen = new Set<number>([mixedPort, apiPort]);
    for (const row of extra) {
      if (seen.has(row.port)) {
        setError(t("settings.dupPort", { n: row.port }));
        return;
      }
      seen.add(row.port);
    }
    applyingRef.current = true;
    const generationAtStart = applyGenerationRef.current;
    setBusy(true);
    setError(null);
    let succeeded = false;
    try {
      const s = await updateSettings({
        mixedPort,
        allowLan,
        apiPort,
        apiSecretEnabled,
        extraInbounds: extra,
        probeUrl: probe.trim() || null,
        tunStack: tunStack.trim() || "mixed",
        tunIpv6Enabled: tunIpv6,
        blockQuic,
        bypassLan,
      });
      setSettings(s);
      // These options are consumed when sing-box starts; apply them together.
      const status = await getProxyStatus().catch(() => null);
      if (status?.running) {
        await restartProxy();
      }
      succeeded = true;
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      applyingRef.current = false;
      setBusy(false);
      // Re-queue only if either this attempt landed cleanly (so a still-dirty
      // draft is a genuinely new edit) or the user changed something else
      // while it was in flight. A failing attempt whose draft never changes
      // (e.g. sing-box can't bind the LAN listener) must not retry itself
      // forever with no backoff — that is the restart-loop bug this guards.
      if (succeeded || applyGenerationRef.current !== generationAtStart) {
        void autoApplyRef.current();
      }
    }
  }, [allowLan, api, apiSecretEnabled, blockQuic, bypassLan, extra, mixed, probe, settings, t, tunIpv6, tunStack]);

  autoApplyRef.current = autoApplyNetwork;

  // Debounce so typing a port number doesn't restart the core per keystroke;
  // toggles / selects / modal saves settle within the same short window.
  useEffect(() => {
    if (!settings) return;
    applyGenerationRef.current += 1;
    const timer = setTimeout(() => void autoApplyRef.current(), 600);
    return () => clearTimeout(timer);
    // Fire on any draft change; autoApplyNetwork itself decides if there is
    // anything valid and dirty to commit.
  }, [settings, mixed, allowLan, api, apiSecretEnabled, probe, tunStack, tunIpv6, blockQuic, bypassLan, extra]);

  // —— Extra inbound listeners (draft rows + modal editor) ——

  function openAddInbound() {
    setInboundEditId(null);
    setInboundKind("mixed");
    setInboundPort("");
    setInboundLan(false);
    setInboundError(null);
    setInboundOpen(true);
  }

  function openEditInbound(row: ExtraInbound) {
    setInboundEditId(row.id);
    setInboundKind(row.kind);
    setInboundPort(String(row.port));
    setInboundLan(!!row.allow_lan);
    setInboundError(null);
    setInboundOpen(true);
  }

  /** Validate in the modal, then commit to the list (auto-applied + core
   * restart via the debounced effect). */
  function saveInbound() {
    const port = Number(inboundPort);
    if (!Number.isFinite(port) || port < 1 || port > 65535) {
      setInboundError(t("settings.invalidExtraPort"));
      return;
    }
    const others = extra.filter((r) => r.id !== inboundEditId);
    const taken = new Set<number>([
      ...others.map((r) => r.port),
      settings?.mixed_port ?? 0,
      settings?.api_port ?? 0,
    ]);
    if (taken.has(port)) {
      setInboundError(t("settings.dupPort", { n: port }));
      return;
    }
    const entry: ExtraInbound = {
      id: inboundEditId ?? `in-${Math.random().toString(36).slice(2, 10)}`,
      kind: inboundKind,
      port,
      allow_lan: inboundLan,
    };
    setExtra((prev) =>
      inboundEditId == null
        ? [...prev, entry]
        : prev.map((r) => (r.id === inboundEditId ? entry : r)),
    );
    setInboundOpen(false);
  }

  function removeInbound(id: string) {
    setExtra((prev) => prev.filter((r) => r.id !== id));
  }

  async function onCopySecret() {
    const secret = settings?.clash_api_secret;
    if (!secret) return;
    try {
      await navigator.clipboard.writeText(secret);
      setSecretCopied(true);
      window.setTimeout(() => setSecretCopied(false), 1500);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  /** Copy-feedback for the truncated path lines (kernel rows + app card). */
  const [copiedPath, setCopiedPath] = useState<string | null>(null);
  async function onCopyPath(path: string) {
    try {
      await navigator.clipboard.writeText(path);
      setCopiedPath(path);
      window.setTimeout(() => setCopiedPath(null), 1500);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  /** Relative time for the app card's "last check" row (backend stores unix
   *  seconds). */
  function formatCheckedAt(ts: number | null | undefined): string {
    if (!ts) return "—";
    const minutes = Math.floor((Date.now() / 1000 - ts) / 60);
    if (minutes < 1) return t("settings.relJustNow");
    if (minutes < 60) return t("settings.relMinAgo", { n: minutes });
    const hours = Math.floor(minutes / 60);
    if (hours < 24) return t("settings.relHourAgo", { n: hours });
    return t("settings.relDayAgo", { n: Math.floor(hours / 24) });
  }

  /** User-triggered secret rotation; backend restarts a running core so the
   * new secret is live immediately. */
  async function onRegenerateSecret() {
    setError(null);
    setBusy(true);
    try {
      const s = await regenerateApiSecret();
      setSettings(s);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setBusy(false);
    }
  }

  async function onDownloadCore(kind: CoreKind) {
    setCoreBusyKind(kind);
    setCoreError(null);
    const status = await getProxyStatus().catch(() => null);
    const viaProxy = !!status?.running;
    setCoreProxyAvailable(viaProxy);
    setCoreProgress({
      kind,
      stage: "preparing",
      downloaded: 0,
      total: null,
      percent: null,
      via_proxy: viaProxy,
    });
    try {
      await downloadCore(kind, null);
      await reloadCore();
    } catch (e) {
      setCoreError(typeof e === "string" ? e : String(e));
    } finally {
      setCoreBusyKind(null);
      setCoreProgress(null);
    }
  }

  async function onCheckCoreUpdate(kind: CoreKind) {
    await runCoreUpdateCheck(kind, cores[kind]?.version ?? null, true);
  }

  /** Switch the active core; a running core restarts onto the new binary. */
  async function onSwitchCore(kind: CoreKind) {
    if (settings?.core_type === kind) return;
    setCoreError(null);
    try {
      const s = await setCoreType(kind);
      setSettings(s);
    } catch (e) {
      setCoreError(typeof e === "string" ? e : String(e));
    }
  }

  /** One compact version row per core (sing-box / Xray / mihomo): identity
   *  line, bundled/latest meta + actions, then a quiet path foot line. */
  function renderCoreRow(kind: CoreKind) {
    const info = cores[kind];
    const busy = coreBusyKind === kind;
    const checking = coreCheckingKind === kind;
    const active = (settings?.core_type ?? "singbox") === kind;
    // Progress events carry the core kind; each row shows only its own.
    const progress =
      coreProgress && (coreProgress.kind ?? "singbox") === kind
        ? coreProgress
        : null;
    return (
      <div
        className={`kernel-row${active ? " core-active" : ""}`}
        key={kind}
        title={
          kind === "xray"
            ? t("settings.coreHintXray")
            : kind === "mihomo"
              ? t("settings.coreHintMihomo")
              : t("settings.coreHint")
        }
      >
        <div className="kernel-row-main">
          {/* Monogram tile: cube = sing-box, bolt = Xray, cat head = mihomo. */}
          <div className="ver-mark kernel-mark" aria-hidden>
            <CoreMark kind={kind} />
          </div>
          <span className="kernel-name">
            {info?.name ?? (kind === "xray" ? "Xray" : kind === "mihomo" ? "mihomo" : "sing-box")}
          </span>
          {/* Radio-style enable: clicking switches the active core (a
             running core restarts onto the new binary). */}
          <button
            type="button"
            className={`core-radio${active ? " on" : ""}`}
            aria-pressed={active}
            aria-label={t("settings.coreUse")}
            title={t("settings.coreUse")}
            disabled={coreBusyKind != null}
            onClick={() => void onSwitchCore(kind)}
          >
            <span className="core-radio-dot" aria-hidden />
          </button>
          {/* Fixed-width slot on BOTH rows (empty when active) so the pill /
             platform columns line up between the two cores. */}
          <span className="kernel-switch-hint muted">
            {active ? "" : t("settings.coreSwitchHint")}
          </span>
          {info?.installed ? (
            !active && (
              <span className={`pill ${info.source === "bundled" ? "ok" : ""}`}>
                {info.source === "bundled"
                  ? t("settings.coreBundled")
                  : t("settings.coreInstalled")}
              </span>
            )
          ) : (
            <span className="pill warn">{t("settings.coreMissing")}</span>
          )}
          <span className="kernel-platform muted mono">
            {info?.platform ?? "…"}
          </span>
          <span className="kernel-version mono">
            {info?.version ?? "—"}
            {info?.source === "downloaded" ? (
              <span className="pill">{t("settings.coreUser")}</span>
            ) : null}
          </span>
        </div>

        <div className="kernel-row-meta">
          <span className="kernel-meta-item mono">
            {t("settings.coreBundledShort")} {info?.bundled_version ?? "—"}
          </span>
          <span className="kernel-meta-sep" aria-hidden>
            ·
          </span>
          <span className="kernel-meta-item mono">
            {t("settings.coreLatestShort")} {info?.latest_version ?? "—"}
          </span>
          {info?.update_available ? (
            <span className="pill warn">{t("settings.coreUpdateAvail")}</span>
          ) : null}
          <div className="kernel-row-actions">
            <GlassButton
              icon="↻"
              disabled={busy || checking || !info}
              onClick={() => void onCheckCoreUpdate(kind)}
            >
              {checking
                ? t("settings.coreChecking")
                : t("settings.coreCheck")}
            </GlassButton>
            <GlassButton
              variant="primary"
              icon="⤓"
              disabled={busy || checking}
              onClick={() => void onDownloadCore(kind)}
            >
              {busy
                ? t("settings.coreDownloading")
                : info?.source === "downloaded"
                  ? info.update_available
                    ? t("settings.coreUpdate")
                    : t("settings.coreRedownload")
                  : t("settings.coreDownload")}
            </GlassButton>
          </div>
        </div>

        {busy && progress && (
          <div className="core-download-progress" aria-live="polite">
            <div className="core-download-progress-head">
              <span className="lat-spinner" aria-hidden />
              <span>
                {progress.stage === "preparing"
                  ? t("settings.corePreparing")
                  : progress.stage === "installing"
                    ? t("settings.coreInstalling")
                    : t("settings.coreDownloading")}
              </span>
              <span className="mono core-download-percent">
                {progress.percent != null
                  ? `${progress.percent}%`
                  : "…"}
              </span>
            </div>
            <div
              className={`core-progress-track${progress.percent == null ? " indeterminate" : ""}`}
            >
              <span
                style={{
                  width: `${progress.percent ?? 24}%`,
                }}
              />
            </div>
            {progress.downloaded > 0 && (
              <div className="muted mono core-download-bytes">
                {fmtCoreBytes(progress.downloaded)}
                {progress.total
                  ? ` / ${fmtCoreBytes(progress.total)}`
                  : ""}
              </div>
            )}
          </div>
        )}

        <div className="kernel-row-foot">
          {active && coreProxyAvailable && (
            <span className="kernel-run">
              <span className="kernel-run-dot" aria-hidden />
              {t("dashboard.coreRunning")}
            </span>
          )}
          {info?.path && (
            <>
              <code className="kernel-path mono" title={info.path}>
                {info.path}
              </code>
              <GlassButton
                iconOnly
                icon={copiedPath === info.path ? "✓" : "⧉"}
                className="kernel-copy-btn"
                title={copiedPath === info.path ? t("common.copied") : t("common.copy")}
                aria-label={t("common.copy")}
                onClick={() => void onCopyPath(info.path!)}
              />
            </>
          )}
        </div>
      </div>
    );
  }

  async function patchApp(partial: Parameters<typeof updateSettings>[0]) {
    setError(null);
    try {
      const s = await updateSettings(partial);
      setSettings(s);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
      try {
        const s = await getSettings();
        setSettings(s);
      } catch {
        /* ignore */
      }
    }
  }

  async function onChangeLocale(next: Locale) {
    if (next === locale) return;
    setError(null);
    try {
      await setLocale(next);
      const s = await getSettings();
      setSettings(s);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  async function onChangeTheme(next: ThemeId) {
    if (next === theme) return;
    setError(null);
    try {
      await setTheme(next);
      const s = await getSettings();
      setSettings(s);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  const customRuntime = (settings?.runtime_source ?? "").startsWith("singbox:");
  /** Xray has no Clash API / clash_api_secret concept — hide that control. */
  const xrayCore = (settings?.core_type ?? "singbox") === "xray";

  useEffect(() => {
    if (customRuntime && CUSTOM_BLOCKED_TABS.has(tab)) {
      setTab("app");
    }
  }, [customRuntime, tab]);

  const visibleTab =
    customRuntime && CUSTOM_BLOCKED_TABS.has(tab) ? "app" : tab;

  // The sponsor easter egg renders on the app tab only; leaving the tab
  // unmounts it — reset the open state so coming back doesn't resurrect
  // a stale panel.
  useEffect(() => {
    if (visibleTab !== "app") setSponsorOpen(false);
  }, [visibleTab]);

  const needsSettings =
    visibleTab === "app" || visibleTab === "ports" || visibleTab === "core";
  if (needsSettings && !settings && !error) {
    return <div className="page empty">{t("common.loading")}</div>;
  }

  const activeTab = tabs.find((x) => x.id === visibleTab)!;

  return (
    <div className="page settings-page settings-wide">
      <header className="page-header">
        <div>
          <h1>{t("settings.title")}</h1>
          <p className="page-desc">{activeTab.hint}</p>
        </div>
      </header>

      {/* Corner links moved into the version tab's app card (project home +
       * sponsor easter egg live next to the app's own version info). */}

      <GlassSeg
        value={visibleTab}
        ariaLabel="Settings sections"
        onChange={(v) => {
          if (customRuntime && CUSTOM_BLOCKED_TABS.has(v)) return;
          setTab(v as SettingsTab);
          setError(null);
        }}
        disabledValues={customRuntime ? CUSTOM_BLOCKED_TABS : undefined}
        titles={
          customRuntime
            ? {
                rules: t("config.customDisabled"),
                chain: t("config.customDisabled"),
                dns: t("config.customDisabled"),
                hosts: t("config.customDisabled"),
              }
            : undefined
        }
        options={tabs.map((x) => ({ value: x.id, label: x.label }))}
      />

      {error &&
        visibleTab !== "rules" &&
        visibleTab !== "chain" &&
        visibleTab !== "dns" &&
        visibleTab !== "hosts" && (
        <ErrorModal message={error} onClose={() => setError(null)} />
      )}

      {/* key={tab} remounts on tab switch → triggers the page-enter fade/slide. */}
      <div
        className={`page-enter${
          visibleTab === "app"
            ? " settings-app-page"
            : visibleTab === "ports"
              ? " settings-ports-page"
              : ""
        }${
          visibleTab === "rules" || visibleTab === "chain" || visibleTab === "dns"
            ? " settings-scroll-embed"
            : ""
        }`}
        key={visibleTab}
      >
        {!customRuntime && visibleTab === "rules" && <RulesPage embedded />}

        {!customRuntime && visibleTab === "chain" && <ChainPage embedded />}
        {!customRuntime && visibleTab === "dns" && (
          <DnsPage embedded />
        )}
        {!customRuntime && visibleTab === "hosts" && <HostsPage embedded />}
      {visibleTab === "app" && settings && (
        <section className="settings-panel" aria-label="Application">
          <div className="card settings-app-card">
            <div className="settings-app-cols">
              <div className="settings-app-col">
              <div className="settings-app-row settings-app-pref">
                <div className="settings-app-text">
                  <div className="settings-app-title">{t("settings.language")}</div>
                  <div className="settings-app-desc muted">
                    {t("settings.languageDesc")}
                  </div>
                </div>
                <GlassSeg
                  value={locale}
                  ariaLabel={t("settings.language")}
                  disabled={busy}
                  onChange={(v) => void onChangeLocale(v as Locale)}
                  options={[
                    { value: "zh", label: t("settings.langZh") },
                    { value: "en", label: t("settings.langEn") },
                  ]}
                />
              </div>
              <AppToggle
                title={t("settings.launchAtLogin")}
                desc={t("settings.launchAtLoginDesc")}
                checked={!!settings?.launch_at_login}
                disabled={busy}
                onChange={(v) => void patchApp({ launchAtLogin: v })}
              />
              <AppToggle
                title={t("settings.silentStart")}
                desc={t("settings.silentStartDesc")}
                checked={!!settings?.silent_start}
                disabled={busy}
                onChange={(v) => void patchApp({ silentStart: v })}
              />
              <AppToggle
                title={t("settings.autoStartProxy")}
                desc={t("settings.autoStartProxyDesc")}
                checked={!!settings?.auto_start_proxy}
                disabled={busy}
                onChange={(v) => void patchApp({ autoStartProxy: v })}
              />
              <AppToggle
                title={t("settings.closeToTray")}
                desc={t("settings.closeToTrayDesc")}
                checked={settings?.close_to_tray !== false}
                disabled={busy}
                onChange={(v) => void patchApp({ closeToTray: v })}
              />
              <AppToggle
                title={t("settings.unloadUi")}
                desc={t("settings.unloadUiDesc")}
                checked={!!settings?.unload_ui_on_tray}
                disabled={busy}
                onChange={(v) => void patchApp({ unloadUiOnTray: v })}
              />
              <AppToggle
                title={t("settings.closeOnSwitch")}
                desc={t("settings.closeOnSwitchDesc")}
                checked={!!settings?.close_connections_on_switch}
                disabled={busy || (settings?.runtime_source ?? "").startsWith("singbox:")}
                onChange={(v) => void patchApp({ closeConnectionsOnSwitch: v })}
              />
              <AppToggle
                title={t("settings.findProcess")}
                desc={t("settings.findProcessDesc")}
                checked={settings?.find_process !== false}
                disabled={busy || (settings?.runtime_source ?? "").startsWith("singbox:")}
                onChange={(v) => void patchApp({ findProcess: v })}
              />
              </div>
              <div className="settings-app-col">
              <div className="settings-app-row settings-app-pref">
                <div className="settings-app-text">
                  <div className="settings-app-title">{t("settings.theme")}</div>
                  <div className="settings-app-desc muted">
                    {t("settings.themeDesc")}
                  </div>
                </div>
                <GlassSeg
                  value={theme}
                  ariaLabel={t("settings.theme")}
                  disabled={busy}
                  onChange={(v) => void onChangeTheme(v as ThemeId)}
                  options={[
                    { value: "aerospace", label: t("settings.themeAerospace") },
                    { value: "day", label: t("settings.themeDay") },
                  ]}
                />
              </div>
              <div className="settings-app-row settings-app-pref settings-hero-row settings-duo-col">
                <div className="settings-app-text">
                  <div className="settings-app-title">{t("settings.glassFrost")}</div>
                  <div className="settings-app-desc muted">
                    {t("settings.glassFrostDesc")}
                  </div>
                </div>
                <GlassSeg
                  value={glassFrost ? "frost" : "lite"}
                  ariaLabel={t("settings.glassFrost")}
                  disabled={busy}
                  onChange={(v) => void setGlassFrost(v === "frost")}
                  options={[
                    { value: "lite", label: t("settings.glassFrostLite") },
                    { value: "frost", label: t("settings.glassFrostFull") },
                  ]}
                />
              </div>
              <div className="settings-app-row settings-app-pref settings-accent-row">
                <div className="settings-app-text">
                  <div className="settings-app-title">{t("settings.accent")}</div>
                  <div className="settings-app-desc muted">
                    {t("settings.accentDesc")}
                  </div>
                </div>
                <div
                  className="settings-accent-swatches"
                  role="group"
                  aria-label={t("settings.accent")}
                >
                  {ACCENTS.map((a) => (
                    <button
                      key={a.id}
                      type="button"
                      className={`settings-accent-dot ${accent === a.id ? "active" : ""}`}
                      style={{ background: a[theme], color: a[theme] }}
                      title={t(ACCENT_LABEL_KEY[a.id] ?? "settings.accent")}
                      aria-label={t(ACCENT_LABEL_KEY[a.id] ?? "settings.accent")}
                      aria-pressed={accent === a.id}
                      disabled={busy}
                      onClick={() => void setAccent(a.id)}
                    >
                      {accent === a.id ? (
                        <span className="settings-accent-check">✓</span>
                      ) : (
                        ""
                      )}
                    </button>
                  ))}
                  <button
                    type="button"
                    className={`settings-accent-dot ${isCustomHexAccent(accent) ? "active" : ""}`}
                    style={
                      isCustomHexAccent(accent)
                        ? { background: accent, color: accent }
                        : { background: CUSTOM_DOT_RAINBOW }
                    }
                    title={t("accent.custom")}
                    aria-label={t("accent.custom")}
                    aria-pressed={isCustomHexAccent(accent)}
                    disabled={busy}
                    onClick={() => setAccentPickerOpen(true)}
                  >
                    {isCustomHexAccent(accent) ? (
                      <span className="settings-accent-check">✓</span>
                    ) : (
                      ""
                    )}
                  </button>
                </div>
              </div>
              <div className="settings-app-row settings-app-pref settings-accent-row">
                <div className="settings-app-text">
                  <div className="settings-app-title">{t("settings.glow")}</div>
                  <div className="settings-app-desc muted">
                    {t("settings.glowDesc")}
                  </div>
                </div>
                <div
                  className="settings-accent-swatches"
                  role="group"
                  aria-label={t("settings.glow")}
                >
                  {/* "Follow accent" mirrors the accent's effective shade. */}
                  <button
                    type="button"
                    className={`settings-accent-dot ${glow === "accent" ? "active" : ""}`}
                    style={{
                      background: resolveAccent(accent)[theme],
                      color: resolveAccent(accent)[theme],
                    }}
                    title={t("settings.glowFollow")}
                    aria-label={t("settings.glowFollow")}
                    aria-pressed={glow === "accent"}
                    disabled={busy}
                    onClick={() => void setGlow("accent")}
                  >
                    {glow === "accent" ? (
                      <span className="settings-accent-check">✓</span>
                    ) : (
                      ""
                    )}
                  </button>
                  {ACCENTS.map((a) => (
                    <button
                      key={a.id}
                      type="button"
                      className={`settings-accent-dot ${glow === a.id ? "active" : ""}`}
                      style={{ background: a[theme], color: a[theme] }}
                      title={t(ACCENT_LABEL_KEY[a.id] ?? "settings.glow")}
                      aria-label={t(ACCENT_LABEL_KEY[a.id] ?? "settings.glow")}
                      aria-pressed={glow === a.id}
                      disabled={busy}
                      onClick={() => void setGlow(a.id)}
                    >
                      {glow === a.id ? (
                        <span className="settings-accent-check">✓</span>
                      ) : (
                        ""
                      )}
                    </button>
                  ))}
                  <button
                    type="button"
                    className={`settings-accent-dot ${isCustomHexAccent(glow) ? "active" : ""}`}
                    style={
                      isCustomHexAccent(glow)
                        ? { background: glow, color: glow }
                        : { background: CUSTOM_DOT_RAINBOW }
                    }
                    title={t("accent.custom")}
                    aria-label={t("accent.custom")}
                    aria-pressed={isCustomHexAccent(glow)}
                    disabled={busy}
                    onClick={() => setGlowPickerOpen(true)}
                  >
                    {isCustomHexAccent(glow) ? (
                      <span className="settings-accent-check">✓</span>
                    ) : (
                      ""
                    )}
                  </button>
                </div>
              </div>
              <div className="settings-app-row settings-app-pref settings-hero-row">
                <div className="settings-app-text">
                  <div className="settings-app-title">{t("settings.heroStyle")}</div>
                  <div className="settings-app-desc muted">
                    {t("settings.heroStyleDesc")}
                  </div>
                </div>
                <GlassSeg
                  value={heroStyle}
                  ariaLabel={t("settings.heroStyle")}
                  disabled={busy}
                  onChange={(v) => void setHeroStyle(v as HeroStyle)}
                  options={[
                    { value: "particle", label: t("settings.heroStyleParticle") },
                    { value: "radiance", label: t("settings.heroStyleRadiance") },
                    { value: "classic", label: t("settings.heroStyleClassic") },
                    { value: "smiley", label: t("settings.heroStyleSmiley") },
                  ]}
                />
              </div>
              <div className="settings-app-row settings-app-pref settings-tray-icon-row settings-duo-col">
                <div className="settings-app-text">
                  <div className="settings-app-title">{t("settings.trayIcon")}</div>
                  <div className="settings-app-desc muted">
                    {t("settings.trayIconDesc")}
                  </div>
                </div>
                <TrayIconPicker
                  value={settings?.tray_icon}
                  disabled={busy}
                  aria-label={t("settings.trayIcon")}
                  onChange={(v) => void patchApp({ trayIcon: v })}
                />
              </div>
            </div>
            </div>
          </div>
          <p className="settings-panel-note muted">{t("settings.toggleSaveNote")}</p>
        </section>
      )}

      {visibleTab === "ports" && settings && (
        <section className="settings-panel" aria-label="Ports">
          <div className="settings-ports-columns">
            <div className="card settings-form settings-form-grid">
              <label className="field field-inline field-span-2">
                <span className="field-inline-row">
                  <span className="field-inline-label">{t("settings.mixedPort")}</span>
                  <input
                    autoCapitalize="off"
                    autoCorrect="off"
                    spellCheck={false}
                    value={mixed}
                    disabled={(settings?.runtime_source ?? "").startsWith("singbox:")}
                    onChange={(e) => setMixed(e.target.value)}
                  />
                </span>
                <span className="field-hint muted">{t("settings.mixedPortHint")}</span>
              </label>
              <div className="via-proxy-row field-span-2">
                <div>
                  <div className="sys-proxy-title">{t("settings.allowLan")}</div>
                  <div className="sys-proxy-desc">
                    {t("settings.allowLanDesc")}
                  </div>
                </div>
                <GlassSwitchControl
                  checked={allowLan}
                  title={t("settings.allowLan")}
                  disabled={busy || (settings?.runtime_source ?? "").startsWith("singbox:")}
                  onChange={setAllowLan}
                />
              </div>
              <label className="field field-span-2">
                <span>{t("settings.probeUrl")}</span>
                <input
                  autoCapitalize="off"
                  autoCorrect="off"
                  spellCheck={false}
                  value={probe}
                  onChange={(e) => setProbe(e.target.value)}
                  placeholder="https://…"
                />
              </label>
              <div className="field field-span-2">
                <span>{t("settings.tunStack")}</span>
                <SolidSelect
                  value={tunStack}
                  onChange={setTunStack}
                  aria-label={t("settings.tunStack")}
                  options={[
                    { value: "mixed", label: "mixed" },
                    { value: "system", label: "system" },
                    { value: "gvisor", label: "gvisor" },
                  ]}
                />
                <span className="field-hint muted">
                  {t("settings.tunStackHint")}{" "}
                  <span className="mono">
                    {settings?.tun_enabled
                      ? t("common.enabled")
                      : t("common.disabled")}
                  </span>
                </span>
              </div>
              <div className="via-proxy-row field-span-2">
                <div>
                  <div className="sys-proxy-title">{t("settings.tunIpv6")}</div>
                  <div className="sys-proxy-desc">{t("settings.tunIpv6Desc")}</div>
                </div>
                <GlassSwitchControl
                  checked={tunIpv6}
                  title={t("settings.tunIpv6")}
                  disabled={busy}
                  onChange={setTunIpv6}
                />
              </div>
              <div className="via-proxy-row field-span-2">
                <div>
                  <div className="sys-proxy-title">{t("settings.blockQuic")}</div>
                  <div className="sys-proxy-desc">{t("settings.blockQuicDesc")}</div>
                </div>
                <GlassSwitchControl
                  checked={blockQuic}
                  title={t("settings.blockQuic")}
                  disabled={busy}
                  onChange={setBlockQuic}
                />
              </div>
              <div className="via-proxy-row field-span-2">
                <div>
                  <div className="sys-proxy-title">{t("settings.bypassLan")}</div>
                  <div className="sys-proxy-desc">{t("settings.bypassLanDesc")}</div>
                </div>
                <GlassSwitchControl
                  checked={bypassLan}
                  title={t("settings.bypassLan")}
                  disabled={busy}
                  onChange={setBypassLan}
                />
              </div>
              <div className="field-divider field-span-2" />
              <label className="field field-inline field-span-2">
                <span className="field-inline-row">
                  <span className="field-inline-label">
                    {t("settings.apiPort")}
                    <span className="field-badge field-badge-warn">
                      {t("settings.apiPortBadge")}
                    </span>
                  </span>
                  <input
                    autoCapitalize="off"
                    autoCorrect="off"
                    spellCheck={false}
                    value={api}
                    disabled={(settings?.runtime_source ?? "").startsWith("singbox:")}
                    onChange={(e) => setApi(e.target.value)}
                  />
                </span>
                <span className="field-hint field-hint-warn">
                  {t("settings.apiPortHint")}
                </span>
              </label>
              <div className="via-proxy-row field-span-2">
                <div>
                  <div className="sys-proxy-title">{t("settings.apiSecretEnabled")}</div>
                  <div className="sys-proxy-desc">
                    {t("settings.apiSecretEnabledDesc")}
                  </div>
                </div>
                <GlassSwitchControl
                  checked={apiSecretEnabled}
                  title={t("settings.apiSecretEnabled")}
                  disabled={busy || customRuntime || xrayCore}
                  onChange={setApiSecretEnabled}
                />
              </div>
              {!xrayCore && apiSecretEnabled && (
                <div className="field field-span-2">
                  <span>{t("settings.apiSecret")}</span>
                  <div className="api-secret-row">
                    <input
                      readOnly
                      autoCapitalize="off"
                      autoCorrect="off"
                      spellCheck={false}
                      className="mono api-secret-input"
                      value={settings?.clash_api_secret ?? ""}
                      placeholder={t("settings.apiSecretNone")}
                    />
                    <GlassButton
                      icon={secretCopied ? "✓" : "⧉"}
                      disabled={!settings?.clash_api_secret}
                      onClick={() => void onCopySecret()}
                      title={t("common.copy")}
                    >
                      {secretCopied ? t("common.copied") : t("common.copy")}
                    </GlassButton>
                    <GlassButton
                      icon="↻"
                      disabled={busy || customRuntime}
                      onClick={() => void onRegenerateSecret()}
                      title={t("settings.regenerateSecret")}
                    >
                      {t("settings.regenerateSecret")}
                    </GlassButton>
                  </div>
                  <span className="field-hint muted">
                    {t("settings.apiSecretHint")}
                  </span>
                </div>
              )}
              {netDiagnostics.length > 0 && (
                <div className="field-span-2 diagnostic-banner-list">
                  {netDiagnostics.map((d) => (
                    <div className="diagnostic-banner" key={d.id}>
                      <div className="diagnostic-banner-issue">{d.issue}</div>
                      <div className="diagnostic-banner-suggestion">
                        {d.suggestion}
                      </div>
                    </div>
                  ))}
                </div>
              )}
            </div>
            <div className="card settings-form settings-inbounds-card">
              <div className="settings-network-card-head">
                <div>
                  <strong>{t("settings.extraInbounds")}</strong>
                  <div className="muted">{t("settings.extraInboundsDesc")}</div>
                </div>
                <GlassButton
                  icon="+"
                  disabled={busy || customRuntime || extra.length >= 10}
                  onClick={openAddInbound}
                >
                  {t("settings.addInboundPort")}
                </GlassButton>
              </div>
              <div className="table-wrap inbound-table-wrap">
                <table className="inbound-table">
                  <colgroup>
                    <col style={{ width: 100 }} />
                    <col />
                    <col style={{ width: 60 }} />
                  </colgroup>
                  <thead>
                    <tr>
                      <th>{t("settings.inboundType")}</th>
                      <th>{t("settings.inboundAddr")}</th>
                      <th></th>
                    </tr>
                  </thead>
                  <tbody>
                    {extra.length === 0 ? (
                      <tr>
                        <td colSpan={3} className="muted inbound-empty">
                          {t("settings.extraInboundsEmpty")}
                        </td>
                      </tr>
                    ) : (
                      extra.map((row) => (
                        <tr key={row.id}>
                          <td>
                            <code>{row.kind}</code>
                          </td>
                          <td className="mono">
                            {row.allow_lan ? "0.0.0.0" : "127.0.0.1"}:{row.port}
                          </td>
                          <td>
                            <div className="rule-menu" data-inbound-menu>
                              <button
                                type="button"
                                className="rule-menu-trigger"
                                aria-label={t("common.edit")}
                                aria-haspopup="menu"
                                aria-expanded={menuInboundId === row.id}
                                disabled={busy}
                                onClick={(e) => {
                                  e.stopPropagation();
                                  setMenuInboundId((id) =>
                                    id === row.id ? null : row.id,
                                  );
                                }}
                              >
                                ⋮
                              </button>
                              {menuInboundId === row.id && (
                                <div className="rule-menu-pop" role="menu">
                                  <button
                                    type="button"
                                    role="menuitem"
                                    className="rule-menu-item"
                                    onClick={() => {
                                      setMenuInboundId(null);
                                      openEditInbound(row);
                                    }}
                                  >
                                    {t("common.edit")}
                                  </button>
                                  <button
                                    type="button"
                                    role="menuitem"
                                    className="rule-menu-item danger"
                                    onClick={() => {
                                      setMenuInboundId(null);
                                      removeInbound(row.id);
                                    }}
                                  >
                                    {t("common.delete")}
                                  </button>
                                </div>
                              )}
                            </div>
                          </td>
                        </tr>
                      ))
                    )}
                  </tbody>
                </table>
              </div>
            </div>
          </div>
        </section>
      )}

      {visibleTab === "core" && (
        <section className="settings-panel version-split" aria-label="Version">
          {coreError && (
            <ErrorModal
              message={coreError}
              onClose={() => setCoreError(null)}
            />
          )}

          <div className="version-col">
            <div className="version-block-title">
              {t("settings.coreVersionTitle")}
            </div>
            <div className="card core-card kernel-list">
              {renderCoreRow("singbox")}
              {renderCoreRow("xray")}
              {renderCoreRow("mihomo")}
            </div>
          </div>

          <div className="version-col">
            <div className="version-block-title">
              {t("settings.appVersionTitle")}
            </div>
            <div className="card core-card app-card">
            <div className="ver-hero">
              <div className="ver-mark app-mark" aria-hidden>
                ◈
              </div>
              <div className="ver-id">
                <div className="ver-name">Satelite</div>
                <div className="ver-sub muted">{t("settings.appTagline")}</div>
              </div>
              <div className="ver-side">
                <span className="stat-label">{t("settings.coreCurrent")}</span>
                <div className="ver-ver mono">{appVersion ?? "…"}</div>
              </div>
            </div>

            <div className="ver-grid">
              <div>
                <span className="stat-label">{t("settings.appLatest")}</span>
                <div className="mono ver-stat">
                  {appChecking && !appUpdate
                    ? "…"
                    : (appUpdate?.latest_version ?? "—")}
                  {appUpdate?.update_available ? (
                    <span className="pill warn">
                      {t("settings.coreUpdateAvail")}
                    </span>
                  ) : appUpdate ? (
                    <span className="pill ok">{t("settings.appUpToDate")}</span>
                  ) : null}
                </div>
              </div>
              <div className="ver-grid-actions">
                <GlassButton
                  icon="↻"
                  disabled={appChecking}
                  onClick={() => void runAppUpdateCheck(true, true)}
                >
                  {appChecking
                    ? t("settings.coreChecking")
                    : t("settings.coreCheck")}
                </GlassButton>
                {/* The app has no in-app downloader — "re-download" simply
                   opens the latest GitHub release page in the browser. */}
                <GlassButton
                  variant="primary"
                  icon="⤓"
                  onClick={() => void openUrl(RELEASES_URL)}
                >
                  {t("settings.coreRedownload")}
                </GlassButton>
              </div>
            </div>

            {appError && (
              <ErrorModal
                message={appError}
                onClose={() => setAppError(null)}
              />
            )}

            {/* Quiet key/value rows filling the equal-height card: last
               update check, host platform (the core binary targets it),
               build stack. */}
            <div className="app-info-list">
              <div className="app-info-row">
                <span className="app-info-label">
                  {t("settings.appCheckedLabel")}
                </span>
                <span className="app-info-value">
                  {appChecking ? "…" : formatCheckedAt(appUpdate?.checked_at)}
                </span>
              </div>
              <div className="app-info-row">
                <span className="app-info-label">
                  {t("settings.appPlatformLabel")}
                </span>
                <span className="app-info-value">
                  {cores.singbox?.platform ?? cores.xray?.platform ?? cores.mihomo?.platform ?? "—"}
                </span>
              </div>
              <div className="app-info-row">
                <span className="app-info-label">
                  {t("settings.appStackLabel")}
                </span>
                <span className="app-info-value">Tauri · React · Rust</span>
              </div>
            </div>

            {appPath && (
              <div className="kernel-row-foot">
                <code className="kernel-path mono" title={appPath}>
                  {appPath}
                </code>
                <GlassButton
                  iconOnly
                  icon={copiedPath === appPath ? "✓" : "⧉"}
                  className="kernel-copy-btn"
                  title={copiedPath === appPath ? t("common.copied") : t("common.copy")}
                  aria-label={t("common.copy")}
                  onClick={() => void onCopyPath(appPath)}
                />
              </div>
            )}

            <div className="ver-links">
              <button
                type="button"
                className="corner-link project-link"
                onClick={() => {
                  openUrl(PROJECT_URL).catch((e) =>
                    setError(typeof e === "string" ? e : String(e)),
                  );
                }}
              >
                {t("settings.projectHome")}
              </button>
              <button
                type="button"
                className="corner-link sponsor-link"
                ref={sponsorBtnRef}
                onClick={(e) => {
                  e.stopPropagation();
                  const rect = sponsorBtnRef.current?.getBoundingClientRect();
                  if (rect) {
                    // Popup bounds (QR 216 + padding + session chrome).
                    const PANEL_W = 246;
                    const PANEL_H = 302;
                    const right = Math.min(
                      Math.max(12, window.innerWidth - rect.right),
                      Math.max(12, window.innerWidth - PANEL_W - 12),
                    );
                    const top =
                      rect.top - PANEL_H - 10 >= 12
                        ? rect.top - PANEL_H - 10
                        : Math.min(
                            rect.bottom + 10,
                            window.innerHeight - PANEL_H - 12,
                          );
                    setSponsorPos({ top, right });
                  }
                  setSponsorOpen((v) => {
                    if (!v) {
                      setSponsorSession(
                        Math.floor(Math.random() * 0xffff)
                          .toString(16)
                          .padStart(4, "0"),
                      );
                    }
                    return !v;
                  });
                }}
              >
                {t("settings.sponsor")}
              </button>
            </div>

            {createPortal(
              sponsorOpen && (
                <div
                  className="sponsor-panel sponsor-session"
                  role="dialog"
                  aria-label={t("settings.sponsor")}
                  onClick={(e) => e.stopPropagation()}
                  style={
                    sponsorPos
                      ? {
                          top: sponsorPos.top,
                          right: sponsorPos.right,
                          bottom: "auto",
                        }
                      : undefined
                  }
                >
                  <div className="sponsor-session-bar" aria-hidden="true">
                    <span>session {sponsorSession}</span>
                    <span className="sponsor-cursor" />
                  </div>
                  <div className="sponsor-session-view">
                    <DecryptReveal radius={140} dismissOnLeave>
                      <img
                        className="sponsor-qr"
                        src={buymecoffeeUrl}
                        alt={t("settings.sponsorScan")}
                        draggable={false}
                      />
                    </DecryptReveal>
                  </div>
                  <pre className="sponsor-session-foot" aria-hidden="true">{`payload: beer.qr
; optional :p`}</pre>
                </div>
              ),
              document.body,
            )}
          </div>
          </div>
        </section>
      )}

      {inboundOpen && (
        <div className="modal-backdrop">
          <div className="modal">
            <header className="modal-header">
              <h2>
                {inboundEditId
                  ? t("settings.editInboundTitle")
                  : t("settings.addInboundPort")}
              </h2>
              <button
                type="button"
                className="icon-btn"
                onClick={() => setInboundOpen(false)}
                disabled={busy}
                aria-label={t("common.cancel")}
              >
                ×
              </button>
            </header>
            <form
              className="modal-body"
              onSubmit={(e) => {
                e.preventDefault();
                void saveInbound();
              }}
            >
              <div className="field">
                <span>{t("settings.inboundType")}</span>
                <GlassSeg
                  value={inboundKind}
                  ariaLabel={t("settings.inboundType")}
                  disabled={busy}
                  onChange={(v) => setInboundKind(v as "mixed" | "http")}
                  options={[
                    { value: "mixed", label: "mixed" },
                    { value: "http", label: "http" },
                  ]}
                />
              </div>
              <label className="field">
                <span>{t("settings.portLabel")}</span>
                <input
                  autoCapitalize="off"
                  autoCorrect="off"
                  spellCheck={false}
                  value={inboundPort}
                  onChange={(e) => setInboundPort(e.target.value)}
                  placeholder="8080"
                  disabled={busy}
                  autoFocus
                />
              </label>
              <div className="via-proxy-row">
                <div>
                  <div className="sys-proxy-title">{t("settings.allowLan")}</div>
                  <div className="sys-proxy-desc">{t("settings.allowLanDesc")}</div>
                </div>
                <GlassSwitchControl
                  checked={inboundLan}
                  title={t("settings.allowLan")}
                  disabled={busy}
                  onChange={setInboundLan}
                />
              </div>
              {inboundError && <div className="form-error">{inboundError}</div>}
              <footer className="modal-footer">
                <GlassButton onClick={() => setInboundOpen(false)} disabled={busy}>
                  {t("common.cancel")}
                </GlassButton>
                <GlassButton type="submit" variant="primary" disabled={busy}>
                  {busy ? t("common.saving") : t("common.save")}
                </GlassButton>
              </footer>
            </form>
          </div>
        </div>
      )}

      {accentPickerOpen && (
        <AccentColorPickerModal
          current={
            isCustomHexAccent(accent) ? accent : resolveAccent(accent)[theme]
          }
          title={t("settings.accentCustomTitle")}
          applyLabel={t("common.save")}
          cancelLabel={t("common.cancel")}
          onApply={(hex) => {
            setAccentPickerOpen(false);
            void setAccent(hex);
          }}
          onClose={() => setAccentPickerOpen(false)}
        />
      )}

      {glowPickerOpen && (
        <AccentColorPickerModal
          current={
            isCustomHexAccent(glow)
              ? glow
              : resolveAccent(glow === "accent" ? accent : glow)[theme]
          }
          title={t("settings.glowCustomTitle")}
          applyLabel={t("common.save")}
          cancelLabel={t("common.cancel")}
          onPreview={(hex) => applyGlowToDom(hex, accent, theme)}
          onRestore={() => applyGlowToDom(glow, accent, theme)}
          onApply={(hex) => {
            setGlowPickerOpen(false);
            void setGlow(hex);
          }}
          onClose={() => setGlowPickerOpen(false)}
        />
      )}
      </div>
    </div>
  );
}

/** Order-sensitive equality for the extra-inbound draft list. */
function sameInbounds(a: ExtraInbound[], b: ExtraInbound[]) {
  if (a.length !== b.length) return false;
  return a.every((x, i) => {
    const y = b[i];
    return (
      x.id === y.id &&
      x.kind === y.kind &&
      x.port === y.port &&
      !!x.allow_lan === !!y.allow_lan
    );
  });
}

function AppToggle({
  title,
  desc,
  checked,
  disabled,
  onChange,
}: {
  title: string;
  desc: string;
  checked: boolean;
  disabled?: boolean;
  onChange: (v: boolean) => void;
}) {
  return (
    <div className="settings-app-row">
      <div className="settings-app-text">
        <div className="settings-app-title">{title}</div>
        <div className="settings-app-desc muted">{desc}</div>
      </div>
      <GlassSwitchControl
        checked={checked}
        title={title}
        disabled={disabled}
        onChange={onChange}
      />
    </div>
  );
}
