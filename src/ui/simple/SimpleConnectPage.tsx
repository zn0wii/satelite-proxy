import { useCallback, useEffect, useState } from "react";
import {
  getProxyStatus,
  getSettings,
  listAllNodes,
  listSubscriptions,
  peekProxyStatus,
  startProxy,
  stopProxy,
  testNodesLatency,
} from "../../api";
import { GlassSeg } from "../../components/GlassSeg";
import { ErrorModal } from "../../components/ErrorModal";
import { EarthGlobeLazy } from "../../components/EarthGlobeLazy";
import { HeroVisual } from "../../components/HeroVisual";
import { useCaptureModeSwitch } from "../../hooks/useCaptureModeSwitch";
import { useVisibleInterval } from "../../hooks/useVisibleInterval";
import { useI18n } from "../../i18n";
import { useTheme } from "../../theme";
import type {
  ProxyNode,
  ProxyStatus,
  SubscriptionView,
} from "../../types";

function fmtSpeed(bps: number) {
  if (bps < 1024) return `${bps} B/s`;
  if (bps < 1024 * 1024) return `${(bps / 1024).toFixed(1)} KB/s`;
  return `${(bps / (1024 * 1024)).toFixed(2)} MB/s`;
}

function fmtBytes(n: number) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / (1024 * 1024)).toFixed(1)} MB`;
  return `${(n / (1024 * 1024 * 1024)).toFixed(2)} GB`;
}

function fmtLatency(ms?: number | null) {
  if (ms == null || ms < 0) return "—";
  return `${ms} ms`;
}

interface Props {
  onGoServers?: () => void;
  onGoTraffic?: () => void;
}

export function SimpleConnectPage({ onGoServers, onGoTraffic }: Props) {
  const { t } = useI18n();
  const { heroIsGlobe } = useTheme();
  // Seed from the cross-mount status snapshot (see api.ts) so switching
  // between simple tabs paints the real state instead of flashing defaults.
  const [proxy, setProxy] = useState<ProxyStatus | null>(() =>
    peekProxyStatus(),
  );
  const [node, setNode] = useState<ProxyNode | null>(null);
  const [nodeCount, setNodeCount] = useState(0);
  const [subs, setSubs] = useState<SubscriptionView[]>([]);
  const [nodeReady, setNodeReady] = useState(false);
  const [busy, setBusy] = useState(false);
  const [testing, setTesting] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [nowSec, setNowSec] = useState(() => Math.floor(Date.now() / 1000));

  const reloadStatus = useCallback(async () => {
    try {
      const status = await getProxyStatus().catch(() => null);
      setProxy(status);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }, []);

  const reloadNode = useCallback(async () => {
    try {
      const [settings, nodes, subList] = await Promise.all([
        getSettings().catch(() => null),
        listAllNodes().catch(() => [] as ProxyNode[]),
        listSubscriptions().catch(() => [] as SubscriptionView[]),
      ]);
      const id = settings?.current_node_id;
      setNode(id ? (nodes.find((n) => n.id === id) ?? null) : nodes[0] ?? null);
      setNodeCount(nodes.length);
      setSubs(subList);
      setNodeReady(true);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
      setNodeReady(true);
    }
  }, []);

  const reload = useCallback(async () => {
    const statusP = reloadStatus();
    const nodeP = reloadNode();
    await statusP;
    await nodeP;
  }, [reloadStatus, reloadNode]);

  const probeCurrent = useCallback(async (nodeId: string) => {
    setTesting(true);
    setNode((prev) =>
      prev && prev.id === nodeId
        ? { ...prev, latency_ms: undefined, latency_at: undefined }
        : prev,
    );
    try {
      const batch = await testNodesLatency([nodeId], 3000);
      const r = batch.results.find((x) => x.id === nodeId);
      if (r) {
        setNode((prev) =>
          prev && prev.id === nodeId
            ? {
                ...prev,
                latency_ms: r.latency_ms ?? null,
                latency_at: r.tested_at,
              }
            : prev,
        );
      }
    } catch {
      /* keep prior / cleared state */
    } finally {
      setTesting(false);
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  useVisibleInterval(() => {
    setNowSec(Math.floor(Date.now() / 1000));
    return reloadStatus();
  }, 1000);

  const onCaptureError = useCallback((msg: string) => {
    setError(msg);
  }, []);

  const { captureMode, captureBusy, requestCaptureMode } = useCaptureModeSwitch(
    proxy,
    setProxy,
    onCaptureError,
  );

  const running = proxy?.running ?? false;
  const connecting =
    busy ||
    proxy?.core_state === "starting" ||
    proxy?.core_state === "stopping";
  const isError = proxy?.core_state === "error" || (!!proxy?.error && !running);
  const orbitState = connecting
    ? "switching"
    : running
      ? "live"
      : isError
        ? "error"
        : "stopped";
  const stateUpper = running
    ? "RUNNING"
    : connecting
      ? proxy?.core_state === "stopping" || (busy && running)
        ? "STOPPING"
        : "STARTING"
      : isError
        ? "ERROR"
        : "STOPPED";
  const dotClass = running ? "on" : connecting ? "busy" : "off";

  const customRuntime = proxy?.runtime_source === "singbox";

  async function onToggle() {
    if (busy || connecting) return;
    setBusy(true);
    setError(null);
    try {
      if (running) {
        setProxy(await stopProxy());
        await reload();
      } else {
        const enableSys = proxy?.system_proxy ?? true;
        setProxy(await startProxy(enableSys));
        await reload();
        const id = node?.id;
        if (id) void probeCurrent(id);
      }
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setBusy(false);
    }
  }

  const up = proxy?.upload_speed ?? 0;
  const down = proxy?.download_speed ?? 0;
  const conns = proxy?.connections ?? 0;

  const enabledSubs = subs.filter((s) => s.enabled);
  const enabledNodeCount = enabledSubs.reduce((sum, s) => sum + s.node_count, 0);

  const startedAt = proxy?.core_started_at ?? null;
  const uptimeLabel =
    running && startedAt != null && startedAt > 0
      ? fmtUptime(nowSec - startedAt)
      : "—";

  const heroTitle = !nodeReady && running
    ? null
    : customRuntime
      ? t("dashboard.customMode", {
          name: proxy?.runtime_profile_name || t("config.singbox"),
        })
      : running
        ? node?.name ?? t("dashboard.disconnected")
        : isError
          ? t("dashboard.errorTitle")
          : t("dashboard.disconnected");

  const heroSub = !nodeReady && running
    ? null
    : customRuntime
      ? t("config.singboxReadonly")
      : running
        ? [node?.protocol?.toUpperCase(), testing ? "…" : fmtLatency(node?.latency_ms)]
            .filter(Boolean)
            .join(" · ")
        : t("dashboard.desc");

  return (
    <div className="simple-page simple-connect">
      {error && (
        <ErrorModal message={error} onClose={() => setError(null)} />
      )}

      <section className={`dash-hero simple-dash-hero is-${orbitState}`}>
        <button
          type="button"
          className="simple-orbit-btn"
          disabled={busy || connecting}
          onClick={() => void onToggle()}
          aria-label={running ? t("dashboard.stop") : t("dashboard.start")}
          aria-pressed={running}
        >
          {heroIsGlobe ? (
            <EarthGlobeLazy interactive={false} />
          ) : (
            <HeroVisual
              state={orbitState}
              spinning={running || connecting}
              switching={connecting}
              variant="simple"
            />
          )}
        </button>
        <div className="dash-hero-copy simple-dash-copy">
          <div className="dash-kicker mono">
            <span className={`status-dot ${dotClass}`} />
            {stateUpper}
            {running && (
              <>
                <span className="dash-kicker-sep">·</span>
                {uptimeLabel}
              </>
            )}
          </div>
          <h1 className="dash-hero-title">
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
        </div>
        {/* Contextual info centered under the icon: running → live traffic;
            stopped → enabled profile + capture mode. */}
        {running ? (
          <button
            type="button"
            className="simple-hero-traffic mono"
            onClick={() => onGoTraffic?.()}
            aria-label={t("dashboard.cardTraffic")}
          >
            <div className="sht-speeds">
              <span>
                <span className="tr-dir down">↓</span>
                {fmtSpeed(down)}
              </span>
              <span>
                <span className="tr-dir up">↑</span>
                {fmtSpeed(up)}
              </span>
            </div>
            <div className="sht-meta">
              Σ {fmtBytes((proxy?.upload_total ?? 0) + (proxy?.download_total ?? 0))}
              {" · "}
              {t("simple.sparkConns", { n: conns })}
            </div>
          </button>
        ) : (
          <div className="simple-hero-stopped">
            <button
              type="button"
              className="simple-hero-subs"
              onClick={() => onGoServers?.()}
            >
              {!nodeReady ? (
                <span className="skel skel-inline skel-w-50" aria-hidden />
              ) : customRuntime ? (
                (proxy?.runtime_profile_name || t("config.singbox"))
              ) : enabledSubs.length > 0 ? (
                <>
                  {enabledSubs.map((s) => s.name).join(" · ")}
                  <span className="shs-count">
                    {t("simple.subNodes", { n: enabledNodeCount })}
                  </span>
                </>
              ) : (
                t("simple.pickSub")
              )}
            </button>
            <div className="simple-hero-capture">
              <span
                className={`dash-inline-label${captureBusy ? " dash-smart-probing" : ""}`}
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
                ready={!!proxy}
                ariaLabel={t("dashboard.capture")}
                disabled={!proxy}
                disabledValues={
                  new Set(
                    [
                      customRuntime || (nodeCount === 0 && captureMode !== "tun")
                        ? "tun"
                        : null,
                      customRuntime && !proxy?.custom_inbound_port ? "system" : null,
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
          </div>
        )}
      </section>
    </div>
  );
}

function fmtUptime(sec: number) {
  if (sec < 0 || !Number.isFinite(sec)) return "—";
  const s = Math.floor(sec);
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const r = s % 60;
  const mm = String(m).padStart(2, "0");
  const ss = String(r).padStart(2, "0");
  if (h > 0) return `${h}:${mm}:${ss}`;
  return `${m}:${ss}`;
}
