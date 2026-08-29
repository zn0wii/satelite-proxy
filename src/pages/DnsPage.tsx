import { useCallback, useEffect, useState, type ReactNode } from "react";
import {
  diagnoseDns,
  getDnsSettings,
  updateDnsSettings,
} from "../api";
import { GlassButton } from "../components/GlassButton";
import { GlassSeg } from "../components/GlassSeg";
import { GlassSwitchControl } from "../components/GlassSwitchControl";
import { ErrorModal } from "../components/ErrorModal";
import { useI18n } from "../i18n";
import type {
  DnsDiagAnswer,
  DnsDiagnosisReport,
  DnsFinalStrategy,
  DnsPathStrategy,
  DnsSettings,
} from "../types";

/** Default diagnosis list — foreign sites plus one domestic site so both
 * resolution paths (remote DoH via proxy vs local/domestic) are visible. */
const PRESET_DIAG_DOMAINS = [
  "google.com",
  "x.com",
  "youtube.com",
  "github.com",
  "baidu.com",
];

/** Cap for user-added domains (mirrors the backend's per-run cap). */
const MAX_CUSTOM_DIAG_DOMAINS = 32;

/** localStorage key for user-added diagnosis domains. Pure UI preference —
 * survives page remounts (page switch = remount here) and app restarts, and
 * stays portable-mode aware because the WebView profile moves with the app. */
const DIAG_DOMAINS_STORAGE_KEY = "satelite.dnsDiagDomains";

function loadCustomDiagDomains(): string[] {
  try {
    const raw = localStorage.getItem(DIAG_DOMAINS_STORAGE_KEY);
    if (!raw) return [];
    const parsed: unknown = JSON.parse(raw);
    if (!Array.isArray(parsed)) return [];
    const out: string[] = [];
    for (const item of parsed) {
      if (typeof item !== "string") continue;
      const domain = item.trim();
      if (
        !domain ||
        PRESET_DIAG_DOMAINS.includes(domain) ||
        out.includes(domain)
      ) {
        continue;
      }
      out.push(domain);
    }
    return out.slice(0, MAX_CUSTOM_DIAG_DOMAINS);
  } catch {
    return [];
  }
}

function persistCustomDiagDomains(domains: string[]) {
  try {
    localStorage.setItem(DIAG_DOMAINS_STORAGE_KEY, JSON.stringify(domains));
  } catch {
    // Storage unavailable (private mode / quota) — persistence is best-effort.
  }
}

function formatAnswer(a: DnsDiagAnswer): string {
  if (a.type === 1 || a.type === 28) return `${a.data} · TTL ${a.ttl}`;
  if (a.type === 5) return `CNAME ${a.data}`;
  return `TYPE ${a.type} ${a.data}`;
}

/** Paths that resolve via plaintext/system resolvers — visible to the ISP,
 * so any (especially foreign) domain on them is a DNS-leak risk. */
const LEAK_PATH_STRATEGIES = new Set<DnsPathStrategy>(["local", "domestic"]);

function SettingRow({
  title,
  desc,
  children,
}: {
  title: string;
  desc?: string;
  children: ReactNode;
}) {
  return (
    <div className="dns-setting-row">
      <div className="dns-setting-text">
        <div className="dns-setting-title">{title}</div>
        {desc && <div className="dns-setting-desc">{desc}</div>}
      </div>
      <div className="dns-setting-control">{children}</div>
    </div>
  );
}

export function DnsPage({ embedded = false }: { embedded?: boolean }) {
  const { t } = useI18n();
  const [dns, setDns] = useState<DnsSettings | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // FakeIP detail modal draft state (seeded on open, applied on save).
  const [fakeipOpen, setFakeipOpen] = useState(false);
  const [fiPoolText, setFiPoolText] = useState("");
  const [fiIpv6, setFiIpv6] = useState(false);
  const [fiBypassText, setFiBypassText] = useState("");
  // Diagnostics: persisted custom domains + per-run report state.
  const [diagCustom, setDiagCustom] = useState<string[]>(loadCustomDiagDomains);
  const [diagInput, setDiagInput] = useState("");
  const [diagBusy, setDiagBusy] = useState(false);
  const [singleBusy, setSingleBusy] = useState<string | null>(null);
  const [diagReport, setDiagReport] = useState<DnsDiagnosisReport | null>(null);

  const reload = useCallback(async () => {
    setError(null);
    try {
      const s = await getDnsSettings();
      setDns(s);
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  async function save(next: DnsSettings) {
    setBusy(true);
    setError(null);
    try {
      const s = await updateDnsSettings(next, true);
      setDns(s);
      setFiBypassText((s.fake_ip.bypass || []).join("\n"));
      return true;
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
      return false;
    } finally {
      setBusy(false);
    }
  }

  function patch(partial: Partial<DnsSettings>) {
    if (!dns) return;
    void save({ ...dns, ...partial });
  }

  function openFakeipModal() {
    if (!dns) return;
    setFiPoolText(dns.fake_ip.inet4_range);
    setFiIpv6(dns.fake_ip.inet6_enabled);
    setFiBypassText((dns.fake_ip.bypass || []).join("\n"));
    setFakeipOpen(true);
  }

  async function saveFakeipModal() {
    if (!dns) return;
    const bypass = fiBypassText
      .split(/[\n,]/)
      .map((s) => s.trim().replace(/^\*\./, "").replace(/^\./, ""))
      .filter(Boolean);
    const ok = await save({
      ...dns,
      fake_ip: {
        ...dns.fake_ip,
        inet4_range: fiPoolText.trim() || dns.fake_ip.inet4_range,
        inet6_enabled: fiIpv6,
        bypass,
      },
    });
    if (ok) setFakeipOpen(false);
  }

  const diagAll = [...PRESET_DIAG_DOMAINS, ...diagCustom];

  function addDiagDomain() {
    const raw =
      diagInput
        .trim()
        .replace(/^https?:\/\//, "")
        .split("/")[0]
        ?.split(":")[0]
        ?.trim() ?? "";
    setDiagInput("");
    if (!raw || diagAll.includes(raw)) return;
    const nextCustom = [...diagCustom, raw].slice(
      0,
      MAX_CUSTOM_DIAG_DOMAINS
    );
    setDiagCustom(nextCustom);
    persistCustomDiagDomains(nextCustom);
  }

  function removeDiagDomain(domain: string) {
    const nextCustom = diagCustom.filter((d) => d !== domain);
    setDiagCustom(nextCustom);
    persistCustomDiagDomains(nextCustom);
    setDiagReport((prev) =>
      prev
        ? {
            ...prev,
            results: prev.results.filter((r) => r.domain !== domain),
          }
        : prev
    );
  }

  async function onDiagnose() {
    if (diagAll.length === 0) return;
    setDiagBusy(true);
    setError(null);
    try {
      setDiagReport(await diagnoseDns(diagAll));
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setDiagBusy(false);
    }
  }

  /** Diagnose one row only; merge the fresh result into the existing report
   * so other rows keep their results. */
  async function onDiagnoseOne(domain: string) {
    if (diagBusy || singleBusy) return;
    setSingleBusy(domain);
    setError(null);
    try {
      const fresh = await diagnoseDns([domain]);
      setDiagReport((prev) => {
        if (!prev) return fresh;
        const results = prev.results
          .filter((r) => r.domain !== domain)
          .concat(fresh.results);
        return { ...fresh, results };
      });
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setSingleBusy(null);
    }
  }

  function strategyLabel(strategy: DnsPathStrategy): string {
    const keys = {
      remote: "dns.stRemote",
      domestic: "dns.stDomestic",
      local: "dns.stLocal",
      block: "dns.stBlock",
      hosts: "dns.stHosts",
      fakeip: "dns.stFakeip",
    } as const;
    return t(keys[strategy]);
  }

  function coreLabel(coreType: string): string {
    if (coreType === "singbox") return "sing-box";
    if (coreType === "mihomo") return "mihomo";
    if (coreType === "xray") return "Xray";
    return coreType;
  }

  if (!dns && !error) {
    return (
      <div className={embedded ? "settings-embed empty" : "page empty"}>
        {t("common.loading")}
      </div>
    );
  }
  if (!dns) {
    return (
      <div className={embedded ? "settings-embed" : "page"}>
        {error && (
          <ErrorModal message={error} onClose={() => setError(null)} />
        )}
      </div>
    );
  }

  const wrapClass = embedded ? "settings-embed dns-page" : "page dns-page";
  const resultMap = new Map(
    (diagReport?.results ?? []).map((r) => [r.domain, r] as const)
  );

  return (
    <div className={wrapClass}>
      {!embedded && (
        <header className="page-header">
          <div>
            <h1>{t("dns.title")}</h1>
            <p className="page-desc">{t("dns.desc")}</p>
          </div>
        </header>
      )}

      {error && (
        <ErrorModal message={error} onClose={() => setError(null)} />
      )}

      <div className="dns-stack dns-grid dns-section-settings">
        <section className="card dns-panel dns-cell dns-cell-general">
          <div className="dns-panel-body dns-general-body">
            <SettingRow title={t("dns.hijack")} desc={t("dns.hijackDesc")}>
              <GlassSwitchControl
                checked={dns.hijack}
                title={t("dns.hijack")}
                disabled={busy}
                onChange={(checked) => patch({ hijack: checked })}
              />
            </SettingRow>

            <SettingRow
              title={t("dns.defaultResolve")}
              desc={t("dns.defaultResolveDesc")}
            >
              <GlassSeg
                value={dns.dns_final}
                ariaLabel={t("dns.defaultResolve")}
                disabled={busy}
                onChange={(v) => patch({ dns_final: v as DnsFinalStrategy })}
                options={[
                  { value: "local", label: t("dns.finalLocal") },
                  { value: "domestic", label: t("dns.finalDomestic") },
                  { value: "remote", label: t("dns.finalRemote") },
                ]}
              />
            </SettingRow>

            <SettingRow title={t("dns.cache")} desc={t("dns.cacheDesc")}>
              <GlassSwitchControl
                checked={dns.cache}
                title={t("dns.cache")}
                disabled={busy}
                onChange={(checked) => patch({ cache: checked })}
              />
            </SettingRow>

            <SettingRow title="FakeIP" desc={t("dns.fakeipDesc")}>
              <div className="dns-fakeip-controls">
                <button
                  type="button"
                  className="icon-btn"
                  title={t("dns.fakeipOptions")}
                  disabled={busy}
                  onClick={openFakeipModal}
                >
                  ⋯
                </button>
                <GlassSwitchControl
                  checked={dns.fake_ip.enabled}
                  title="FakeIP"
                  disabled={busy}
                  onChange={(checked) =>
                    void save({
                      ...dns,
                      fake_ip: {
                        ...dns.fake_ip,
                        enabled: checked,
                      },
                    })
                  }
                />
              </div>
            </SettingRow>
          </div>
        </section>

        <section className="card dns-panel dns-cell dns-cell-diag">
          <header className="dns-panel-head">
            <h2>{t("dns.diagTitle")}</h2>
            <p>{t("dns.diagDesc")}</p>
          </header>
          <div className="dns-panel-body">
            <div className="dns-diag-toolbar">
              <div className="dns-test-row dns-diag-add">
                <input
                  autoCapitalize="off"
                  autoCorrect="off"
                  spellCheck={false}
                  placeholder={t("dns.domainPh")}
                  value={diagInput}
                  onChange={(e) => setDiagInput(e.target.value)}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") addDiagDomain();
                  }}
                />
                <GlassButton
                  disabled={diagBusy || !diagInput.trim()}
                  onClick={addDiagDomain}
                >
                  {t("dns.addDomain")}
                </GlassButton>
              </div>
              <GlassButton
                variant="primary"
                icon="⌕"
                disabled={diagBusy || diagAll.length === 0}
                onClick={() => void onDiagnose()}
              >
                {diagBusy ? t("dns.diagRunning") : t("dns.diagRun")}
              </GlassButton>
            </div>

            {diagReport && (
              <div className="dns-diag-meta">
                <strong>{coreLabel(diagReport.core_type)}</strong>
                <span
                  className={`dns-diag-run${diagReport.running ? " on" : ""}`}
                >
                  {diagReport.running
                    ? t("dns.coreRunning")
                    : t("dns.coreStopped")}
                </span>
                {diagReport.notes.map((note) => (
                  <span key={note} className="dns-diag-note">
                    {note}
                  </span>
                ))}
              </div>
            )}

            <div className="table-wrap dns-diag-table-wrap">
              <table className="dns-diag-table">
                <colgroup>
                  <col className="col-domain" />
                  <col className="col-strategy" />
                  <col className="col-match" />
                  <col className="col-server" />
                  <col className="col-query" />
                  <col className="col-actions" />
                </colgroup>
                <thead>
                  <tr>
                    <th>{t("dns.domainLabel")}</th>
                    <th>{t("dns.colStrategy")}</th>
                    <th>{t("dns.matchedLabel")}</th>
                    <th>{t("dns.serverLabel")}</th>
                    <th>{t("dns.coreQuery")}</th>
                    <th></th>
                  </tr>
                </thead>
                <tbody>
                  {diagAll.map((domain) => {
                    const result = resultMap.get(domain);
                    const path = result?.path ?? null;
                    const leak =
                      !!path && LEAK_PATH_STRATEGIES.has(path.strategy);
                    const rowBusy = singleBusy === domain;
                    const query = result?.query ?? null;
                    const firstAnswer = query?.answers[0]?.data ?? null;
                    const queryTitle = query
                      ? (query.error ??
                        query.answers.map(formatAnswer).join("\n"))
                      : "";
                    return (
                      <tr key={domain} className="dns-diag-tr">
                        <td className="mono dns-diag-td-domain">{domain}</td>
                        <td className="dns-diag-td-strategy">
                          {path ? (
                            <span className="dns-diag-strategy-inner">
                              <span
                                className={`dns-diag-pill ${path.strategy}${leak ? " leak" : ""}`}
                                title={
                                  leak
                                    ? path.strategy === "local"
                                      ? t("dns.leakRiskLocal")
                                      : t("dns.leakRiskDomestic")
                                    : (path.note ?? undefined)
                                }
                              >
                                {leak ? "⚠ " : ""}
                                {strategyLabel(path.strategy)}
                              </span>
                              {path.approx && (
                                <span
                                  className="dns-diag-approx"
                                  title={t("dns.approxTag")}
                                >
                                  {t("dns.approxTag")}
                                </span>
                              )}
                            </span>
                          ) : (
                            <span className="muted">—</span>
                          )}
                        </td>
                        <td
                          className="dns-diag-td-match"
                          title={path?.matched_by ?? ""}
                        >
                          {path?.matched_by ?? ""}
                        </td>
                        <td
                          className="mono dns-diag-td-server"
                          title={path?.servers.join("\n") ?? ""}
                        >
                          {path?.servers[0] ?? ""}
                        </td>
                        <td
                          className="dns-diag-td-query"
                          title={queryTitle || (result?.query_note ?? "")}
                        >
                          {query ? (
                            query.error ? (
                              <span className="dns-diag-fail">
                                ✕ {query.status_text || "ERROR"}
                              </span>
                            ) : (
                              <span
                                className={
                                  query.ok ? "dns-diag-ok" : "dns-diag-fail"
                                }
                              >
                                {query.status_text || "OK"}
                                {firstAnswer ? ` · ${firstAnswer}` : ""}
                                {` · ${query.elapsed_ms} ms`}
                                {query.answers.length > 1
                                  ? ` +${query.answers.length - 1}`
                                  : ""}
                              </span>
                            )
                          ) : result?.query_note ? (
                            <span className="muted">{result.query_note}</span>
                          ) : (
                            <span className="muted">—</span>
                          )}
                        </td>
                        <td className="dns-diag-td-actions">
                          <span className="dns-diag-actions-inner">
                            <button
                              type="button"
                              className="icon-btn"
                              title={t("dns.test")}
                              disabled={diagBusy || rowBusy}
                              onClick={() => void onDiagnoseOne(domain)}
                            >
                              {rowBusy ? "…" : "⌕"}
                            </button>
                            {diagCustom.includes(domain) && (
                              <button
                                type="button"
                                className="icon-btn"
                                title={t("dns.removeDomain")}
                                disabled={diagBusy || rowBusy}
                                onClick={() => removeDiagDomain(domain)}
                              >
                                ×
                              </button>
                            )}
                          </span>
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
            </div>
          </div>
        </section>

        {fakeipOpen && (
          <div className="modal-backdrop" onClick={() => setFakeipOpen(false)}>
            <div
              className="modal dns-fakeip-modal"
              onClick={(e) => e.stopPropagation()}
            >
              <header className="modal-header">
                <h2>{t("dns.fakeipOptions")}</h2>
                <button
                  type="button"
                  className="icon-btn"
                  onClick={() => setFakeipOpen(false)}
                >
                  ×
                </button>
              </header>
              <div className="modal-body">
                <label className="field dns-field">
                  <span>{t("dns.ipv4Pool")}</span>
                  <input
                    autoCapitalize="off"
                    autoCorrect="off"
                    spellCheck={false}
                    value={fiPoolText}
                    onChange={(e) => setFiPoolText(e.target.value)}
                  />
                </label>
                <SettingRow
                  title={t("dns.ipv6Fakeip")}
                  desc={t("dns.ipv6FakeipDesc")}
                >
                  <GlassSwitchControl
                    checked={fiIpv6}
                    title={t("dns.ipv6Fakeip")}
                    onChange={setFiIpv6}
                  />
                </SettingRow>
                <label className="field dns-field">
                  <span>{t("dns.bypassSuffix")}</span>
                  <textarea
                    autoCapitalize="off"
                    autoCorrect="off"
                    spellCheck={false}
                    rows={4}
                    value={fiBypassText}
                    onChange={(e) => setFiBypassText(e.target.value)}
                    placeholder={"local\nlan\ninternal"}
                  />
                </label>
                <div className="dns-fakeip-modal-actions">
                  <GlassButton disabled={busy} onClick={() => setFakeipOpen(false)}>
                    {t("common.cancel")}
                  </GlassButton>
                  <GlassButton
                    variant="primary"
                    disabled={busy}
                    onClick={() => void saveFakeipModal()}
                  >
                    {t("common.save")}
                  </GlassButton>
                </div>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  );
}
