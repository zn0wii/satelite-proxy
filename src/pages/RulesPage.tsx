import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type ReactNode,
} from "react";
import { useRulesetDragSort } from "../hooks/useRulesetDragSort";
import { listen } from "@tauri-apps/api/event";
import {
  createRuleSet,
  batchSetRuleTargets,
  deleteRuleSet,
  getRuleSet,
  getSettings,
  listAllNodes,
  listChains,
  listRemoteRuleItems,
  listRuleSets,
  peekSettings,
  removeRule,
  refreshRemoteRuleSet,
  updateRuleSet,
  reorderRuleSets,
  resetRuleSet,
  resetBuiltinRuleSet,
  saveRule,
  setRuleEnabled,
  setRuleSetEnabled,
  setRuleSetDnsStrategy,
  setRuleSetStrategy,
  refreshGeodata,
  updateSettings,
  type GeodataInfo,
} from "../api";
import { GlassButton } from "../components/GlassButton";
import { SolidSelect } from "../components/SolidSelect";
import { GlassSeg } from "../components/GlassSeg";
import { GlassSwitchControl } from "../components/GlassSwitchControl";
import { ErrorModal } from "../components/ErrorModal";
import { useI18n } from "../i18n";
import { extractDomainSuffix } from "./FailuresPage";
import type {
  ProxyChain,
  ProxyNode,
  Rule,
  RuleSetDnsStrategy,
  RuleSetSummary,
  RemoteRulePage,
  RuleTarget,
  RuleType,
} from "../types";

type RouteFinal = "proxy" | "direct" | "block";

/**
 * If `payload` is a pasted http(s) URL, suggest what it would actually
 * resolve to for the given rule type — DOMAIN keeps the full hostname,
 * DOMAIN-SUFFIX collapses to the last two labels (via `extractDomainSuffix`,
 * shared with the failures quick-add-rule flow). Returns null when the input
 * isn't a URL, fails to parse, or the rule type has no sensible suggestion
 * (KEYWORD/IP-CIDR/PROCESS — matching a keyword or literal against a full
 * URL isn't meaningful).
 */
function suggestPayloadFromUrl(payload: string, ruleType: RuleType): string | null {
  if (!/^https?:\/\//i.test(payload.trim())) return null;
  if (ruleType !== "domain" && ruleType !== "domain_suffix") return null;
  let hostname: string;
  try {
    hostname = new URL(payload.trim()).hostname;
  } catch {
    return null;
  }
  if (!hostname) return null;
  const suggestion =
    ruleType === "domain" ? hostname : extractDomainSuffix(hostname);
  return suggestion || null;
}

const REMOTE_PAGE_SIZE = 100;

/** Builtin remote set id → the geodata matcher the Xray / mihomo generators
 *  emit instead of reading the .srs cache (and which geodata file backs it). */
const XRAY_GEODATA_SETS: Record<
  string,
  { matcher: string; dat: "geosite" | "geoip" }
> = {
  "system-geosite-cn": { matcher: "geosite:cn", dat: "geosite" },
  "system-geoip-cn": { matcher: "geoip:cn", dat: "geoip" },
  "system-geolocation-not-cn": {
    matcher: "geosite:geolocation-!cn",
    dat: "geosite",
  },
};

function fmtDatSize(bytes: number) {
  if (bytes >= 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${Math.max(1, Math.round(bytes / 1024))} KB`;
}

function fmtDatTime(unix: number | null) {
  if (!unix) return "—";
  try {
    return new Date(unix * 1000).toLocaleString();
  } catch {
    return "—";
  }
}

interface Props {
  /** Hide page chrome when embedded under Settings. */
  embedded?: boolean;
}

export function RulesPage({ embedded = false }: Props) {
  const { t } = useI18n();
  const [sets, setSets] = useState<RuleSetSummary[]>([]);
  const [viewSetId, setViewSetId] = useState<string | null>(null);
  const [rules, setRules] = useState<Rule[]>([]);
  const [filter, setFilter] = useState("");
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // Seed from the cross-mount settings snapshot so re-mounting on tab switch
  // paints the persisted final directly instead of flashing the default.
  const [routeFinal, setRouteFinal] = useState<RouteFinal>(() => {
    const saved = peekSettings()?.route_final?.toLowerCase();
    return saved === "direct" || saved === "block" ? saved : "proxy";
  });
  const [finalBusy, setFinalBusy] = useState(false);
  // Geodata cores (Xray / mihomo): user-added remote .srs sets are skipped by
  // their generators — the sidebar greys them out (builtin remote sets map
  // onto geosite/geoip matchers backed by geodata files instead).
  const [geoCore, setGeoCore] = useState<"xray" | "mihomo" | null>(() => {
    const ct = peekSettings()?.core_type;
    return ct === "xray" ? "xray" : ct === "mihomo" ? "mihomo" : null;
  });
  const [geodata, setGeodata] = useState<GeodataInfo | null>(null);

  const [editOpen, setEditOpen] = useState(false);
  const [editRule, setEditRule] = useState<Rule | null>(null);
  const [ruleType, setRuleType] = useState<RuleType>("domain_suffix");
  const [payload, setPayload] = useState("");
  const [target, setTarget] = useState<RuleTarget>("proxy");
  const [pinNodeId, setPinNodeId] = useState<string>("");
  const [nodeQuery, setNodeQuery] = useState("");
  const [smartInclude, setSmartInclude] = useState("");
  const [smartExclude, setSmartExclude] = useState("");
  const [chainId, setChainId] = useState<string>("");
  const [chains, setChains] = useState<ProxyChain[]>([]);
  const [nodes, setNodes] = useState<ProxyNode[]>([]);
  const [enabled, setEnabled] = useState(true);
  const [busy, setBusy] = useState(false);

  /** New rule-set modal (window.prompt is unreliable in Tauri WebView). */
  const [newSetOpen, setNewSetOpen] = useState(false);
  const [newSetName, setNewSetName] = useState(t("rules.setNamePh"));
  const [newSetKind, setNewSetKind] = useState<"local" | "remote">("local");
  const [newSetUrl, setNewSetUrl] = useState("");
  const [newSetTarget, setNewSetTarget] = useState<
    "proxy" | "direct" | "block" | "node" | "filter" | "chain"
  >("proxy");
  const [newSetNodeId, setNewSetNodeId] = useState("");
  const [newSetNodeQuery, setNewSetNodeQuery] = useState("");
  const [newSetSmartInclude, setNewSetSmartInclude] = useState("");
  const [newSetSmartExclude, setNewSetSmartExclude] = useState("");
  const [newSetChainId, setNewSetChainId] = useState("");
  const [newSetUpdateInterval, setNewSetUpdateInterval] = useState<
    "disabled" | "1h" | "12h" | "24h"
  >("disabled");
  const [newSetBusy, setNewSetBusy] = useState(false);
  const [editSetTarget, setEditSetTarget] = useState<RuleSetSummary | null>(null);
  const [editSetName, setEditSetName] = useState("");
  const [editSetUrl, setEditSetUrl] = useState("");
  const [editSetUpdateInterval, setEditSetUpdateInterval] = useState<
    "disabled" | "1h" | "12h" | "24h"
  >("disabled");
  const [editSetBusy, setEditSetBusy] = useState(false);
  /** Row ⋮ menu open for this rule id */
  const [menuRuleId, setMenuRuleId] = useState<string | null>(null);
  /** Rule-set card ⋮ menu open for this set id. */
  const [menuSetId, setMenuSetId] = useState<string | null>(null);
  const [remoteBusyIds, setRemoteBusyIds] = useState<Set<string>>(new Set());
  /** Rule-set ids with a background enable/disable restart in flight. */
  const [togglingIds, setTogglingIds] = useState<Set<string>>(new Set());
  const toggleGenRef = useRef<Map<string, number>>(new Map());
  const togglePrevRef = useRef<Map<string, boolean>>(new Map());
  const [remotePage, setRemotePage] = useState<RemoteRulePage | null>(null);
  const [remotePageIndex, setRemotePageIndex] = useState(0);
  const [remoteRulesLoading, setRemoteRulesLoading] = useState(false);
  const [remoteRulesError, setRemoteRulesError] = useState<string | null>(null);
  /** Remote contents are NOT parsed on select — only after the user clicks
   *  "parse & show" (per set selection; resets when switching sets). */
  const [remoteParsed, setRemoteParsed] = useState(false);
  /** Toolbar ⋮ popover (DNS strategy + batch entry). */
  const [toolbarMenuOpen, setToolbarMenuOpen] = useState(false);
  /** Batch set-routes modal state. */
  const [batchOpen, setBatchOpen] = useState(false);
  const [batchBusy, setBatchBusy] = useState(false);
  const [batchTarget, setBatchTarget] = useState<RuleTarget>("proxy");
  const [batchNodeId, setBatchNodeId] = useState("");
  const [batchNodeQuery, setBatchNodeQuery] = useState("");
  const [batchSmartInclude, setBatchSmartInclude] = useState("");
  const [batchSmartExclude, setBatchSmartExclude] = useState("");
  const [batchChainId, setBatchChainId] = useState("");

  const setsRef = useRef(sets);
  setsRef.current = sets;
  const persistLockRef = useRef(false);

  const reloadSets = useCallback(async () => {
    const list = await listRuleSets();
    setSets(list);
    const preferred =
      list.find((s) => s.enabled)?.id ?? list[0]?.id ?? null;
    setViewSetId((prev) =>
      prev && list.some((set) => set.id === prev) ? prev : preferred,
    );
    return { list, preferred };
  }, []);

  const reloadRouteFinal = useCallback(async () => {
    try {
      const s = await getSettings();
      const rf = (s.route_final ?? "proxy").toLowerCase();
      if (rf === "direct" || rf === "block" || rf === "proxy") {
        setRouteFinal(rf);
      }
      {
        const ct = s.core_type ?? "singbox";
        setGeoCore(ct === "xray" ? "xray" : ct === "mihomo" ? "mihomo" : null);
      }
    } catch {
      /* keep default */
    }
  }, []);

  // Geodata cores: card state (file presence/size/mtime), per-kind pair.
  useEffect(() => {
    if (!geoCore) return;
    let disposed = false;
    void refreshGeodata(false, geoCore)
      .then((info) => {
        if (!disposed) setGeodata(info);
      })
      .catch(() => undefined);
    return () => {
      disposed = true;
    };
  }, [geoCore]);

  const onRouteFinalChange = async (next: RouteFinal) => {
    if (next === routeFinal || finalBusy) return;
    setFinalBusy(true);
    setError(null);
    try {
      const s = await updateSettings({ routeFinal: next });
      const rf = (s.route_final ?? next).toLowerCase();
      setRouteFinal(
        rf === "direct" || rf === "block" || rf === "proxy" ? rf : next,
      );
    } catch (e) {
      setError(String(e));
    } finally {
      setFinalBusy(false);
    }
  };

  const reloadRules = useCallback(async (setId: string | null) => {
    if (!setId) {
      setRules([]);
      return;
    }
    const set = await getRuleSet(setId);
    setRules([...set.rules].sort((a, b) => a.ord - b.ord));
  }, []);

  const reload = useCallback(async () => {
    setError(null);
    try {
      await reloadRouteFinal();
      const { list, preferred } = await reloadSets();
      const sid = viewSetId && list.some((set) => set.id === viewSetId)
        ? viewSetId
        : preferred;
      if (sid) {
        setViewSetId(sid);
        await reloadRules(sid);
      }
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    } finally {
      setLoading(false);
    }
  }, [reloadSets, reloadRules, reloadRouteFinal, viewSetId]);

  useEffect(() => {
    void reload();
    void ensureNodesLoaded();
    // Without this, every remount (settings-tab switch) starts with an empty
    // chain list and chain-target rules flash/permanently render as "stale"
    // until some editor modal happens to call ensureChainsLoaded().
    void ensureChainsLoaded();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (viewSetId) void reloadRules(viewSetId);
  }, [viewSetId, reloadRules]);

  useEffect(() => {
    if (!menuRuleId && !menuSetId && !toolbarMenuOpen) return;
    function onDocPointerDown(e: PointerEvent) {
      const t = e.target as HTMLElement | null;
      if (t?.closest?.("[data-rule-menu], [data-ruleset-menu], [data-toolbar-menu]")) return;
      setMenuRuleId(null);
      setMenuSetId(null);
      setToolbarMenuOpen(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        setMenuRuleId(null);
        setMenuSetId(null);
        setToolbarMenuOpen(false);
      }
    }
    document.addEventListener("pointerdown", onDocPointerDown, true);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("pointerdown", onDocPointerDown, true);
      document.removeEventListener("keydown", onKey);
    };
  }, [menuRuleId, menuSetId, toolbarMenuOpen]);

  useEffect(() => {
    // RulesPage remounts on every settings-tab switch; if it unmounts before
    // listen() resolves, dispose immediately — otherwise the listener (and
    // its closure over this page's setters) leaks onto the global bus.
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<{ id: string; status: string; error?: string | null }>(
      "remote-rule-set-status",
      (event) => {
        const { id, status, error: downloadError } = event.payload;
        setRemoteBusyIds((current) => {
          const next = new Set(current);
          if (status === "downloading") next.add(id);
          else next.delete(id);
          return next;
        });
        if (status === "error" && downloadError) setError(downloadError);
        void reloadSets();
      },
    ).then((dispose) => {
      if (disposed) dispose();
      else unlisten = dispose;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [reloadSets]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    void listen<{
      id: string;
      enabled: boolean;
      status: "restarting" | "ready" | "error";
      error?: string | null;
    }>("rule-set-apply-status", (event) => {
      const { id, status, error: applyError } = event.payload;

      if (status === "restarting") {
        setTogglingIds((cur) => new Set(cur).add(id));
        return;
      }

      setTogglingIds((cur) => {
        const next = new Set(cur);
        next.delete(id);
        return next;
      });

      if (status === "ready") {
        void reloadSets();
        return;
      }

      // status === "error": roll back the switch to its pre-click value.
      // The store write already succeeded — only the visual state reverts.
      const prev = togglePrevRef.current.get(id);
      if (prev !== undefined) {
        setSets((list) =>
          list.map((s) => (s.id === id ? { ...s, enabled: prev } : s)),
        );
      }
      setError(applyError ?? t("rules.restartFailed"));
    }).then((dispose) => {
      if (disposed) dispose();
      else unlisten = dispose;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [reloadSets]);

  const filtered = useMemo(() => {
    const q = filter.trim().toLowerCase();
    if (!q) return rules;
    return rules.filter(
      (r) =>
        r.payload.toLowerCase().includes(q) ||
        r.type.toLowerCase().includes(q) ||
        ruleTypeLabel(r.type).toLowerCase().includes(q) ||
        r.target.toLowerCase().includes(q) ||
        (r.node_name ?? "").toLowerCase().includes(q) ||
        (r.smart_include ?? []).some((k) => k.toLowerCase().includes(q)) ||
        (r.smart_exclude ?? []).some((k) => k.toLowerCase().includes(q)),
    );
  }, [rules, filter]);

  const nodeById = useMemo(() => {
    const m = new Map<string, ProxyNode>();
    for (const n of nodes) m.set(n.id, n);
    return m;
  }, [nodes]);

  const chainById = useMemo(() => {
    const m = new Map<string, ProxyChain>();
    for (const c of chains) m.set(c.id, c);
    return m;
  }, [chains]);

  const filteredNodes = useMemo(() => {
    const q = nodeQuery.trim().toLowerCase();
    if (!q) return nodes;
    return nodes.filter(
      (n) =>
        n.name.toLowerCase().includes(q) ||
        n.server.toLowerCase().includes(q) ||
        n.protocol.toLowerCase().includes(q),
    );
  }, [nodes, nodeQuery]);

  /** Node list for the batch modal's picker (own query, no rule-modal state). */
  const batchFilteredNodes = useMemo(() => {
    const q = batchNodeQuery.trim().toLowerCase();
    if (!q) return nodes;
    return nodes.filter(
      (n) =>
        n.name.toLowerCase().includes(q) ||
        n.server.toLowerCase().includes(q) ||
        n.protocol.toLowerCase().includes(q),
    );
  }, [nodes, batchNodeQuery]);

  /** Node list for the new-set modal's picker. */
  const newSetFilteredNodes = useMemo(() => {
    const q = newSetNodeQuery.trim().toLowerCase();
    if (!q) return nodes;
    return nodes.filter(
      (n) =>
        n.name.toLowerCase().includes(q) ||
        n.server.toLowerCase().includes(q) ||
        n.protocol.toLowerCase().includes(q),
    );
  }, [nodes, newSetNodeQuery]);

  /** Split by whitespace (spaces / tabs / newlines). */
  function parseKeywords(raw: string): string[] {
    return raw
      .split(/\s+/)
      .map((s) => s.trim())
      .filter(Boolean);
  }

  /** Keywords present in both the whitelist and the blacklist (conflict). */
  function keywordOverlap(include: string[], exclude: string[]): string[] {
    const out: string[] = [];
    for (const a of include) {
      const al = a.toLowerCase();
      if (exclude.some((b) => b.toLowerCase() === al) && !out.some((x) => x.toLowerCase() === al)) {
        out.push(a);
      }
    }
    return out;
  }

  const batchKeywordOverlap = useMemo(
    () =>
      batchTarget === "smart"
        ? keywordOverlap(parseKeywords(batchSmartInclude), parseKeywords(batchSmartExclude))
        : [],
    [batchTarget, batchSmartInclude, batchSmartExclude],
  );

  const newSetKeywordOverlap = useMemo(
    () =>
      newSetTarget === "filter"
        ? keywordOverlap(parseKeywords(newSetSmartInclude), parseKeywords(newSetSmartExclude))
        : [],
    [newSetTarget, newSetSmartInclude, newSetSmartExclude],
  );

  const smartKeywordOverlap = useMemo(
    () =>
      target === "smart"
        ? keywordOverlap(parseKeywords(smartInclude), parseKeywords(smartExclude))
        : [],
    [target, smartInclude, smartExclude],
  );

  const payloadSuggestion = useMemo(
    () => suggestPayloadFromUrl(payload, ruleType),
    [payload, ruleType],
  );

  /** Node count matching include/exclude keyword filters. Same semantics as
   *  the backend pool: blacklist OR skips, whitelist OR allows, empty
   *  whitelist = every node. */
  function countKeywordMatches(
    list: ProxyNode[],
    include: string[],
    exclude: string[],
  ): number {
    return list.filter((n) => {
      const name = n.name.toLowerCase();
      // Blacklist OR: any hit → skip
      if (exclude.some((k) => name.includes(k.toLowerCase()))) return false;
      // Whitelist OR: empty = allow all; else any hit allows
      if (include.length === 0) return true;
      return include.some((k) => name.includes(k.toLowerCase()));
    }).length;
  }

  const smartMatchCount = useMemo(
    () =>
      target === "smart"
        ? countKeywordMatches(nodes, parseKeywords(smartInclude), parseKeywords(smartExclude))
        : 0,
    [target, smartInclude, smartExclude, nodes],
  );

  const batchSmartMatchCount = useMemo(
    () =>
      batchTarget === "smart"
        ? countKeywordMatches(
            nodes,
            parseKeywords(batchSmartInclude),
            parseKeywords(batchSmartExclude),
          )
        : 0,
    [batchTarget, batchSmartInclude, batchSmartExclude, nodes],
  );

  const newSetSmartMatchCount = useMemo(
    () =>
      newSetTarget === "filter"
        ? countKeywordMatches(
            nodes,
            parseKeywords(newSetSmartInclude),
            parseKeywords(newSetSmartExclude),
          )
        : 0,
    [newSetTarget, newSetSmartInclude, newSetSmartExclude, nodes],
  );

  const viewSet = sets.find((s) => s.id === viewSetId);

  /** The single target a uniform set enforces per rule. Mixed (smart) sets
   *  keep per-rule decisions (null); Filter sets express it as `smart`. */
  const setUniformTarget: RuleTarget | null =
    viewSet?.strategy === "smart"
      ? null
      : viewSet?.strategy === "filter"
        ? "smart"
        : ((viewSet?.strategy ?? "proxy") as RuleTarget);

  /** Plain sets stay uniform: a per-rule target different from the set
   *  strategy must live in a Mixed (smart) set — the editor guides the
   *  conversion instead of saving a silently-diverging rule. */
  const plainDiverged =
    !!viewSet && setUniformTarget !== null && target !== setUniformTarget;

  useEffect(() => {
    setRemotePageIndex(0);
    setRemoteParsed(false);
  }, [viewSetId]);

  useEffect(() => {
    if (!remoteParsed || !viewSet?.remote?.local_path) {
      setRemotePage(null);
      setRemoteRulesError(null);
      setRemoteRulesLoading(false);
      return;
    }
    let cancelled = false;
    const timer = window.setTimeout(() => {
      setRemoteRulesLoading(true);
      setRemoteRulesError(null);
      void listRemoteRuleItems(
        viewSet.id,
        remotePageIndex * REMOTE_PAGE_SIZE,
        REMOTE_PAGE_SIZE,
        filter,
      )
        .then((page) => {
          if (cancelled) return;
          if (page.total > 0 && page.items.length === 0 && remotePageIndex > 0) {
            setRemotePageIndex(0);
            return;
          }
          setRemotePage(page);
          if (!filter.trim()) {
            setSets((current) =>
              current.map((set) =>
                set.id === viewSet.id ? { ...set, rule_count: page.total } : set,
              ),
            );
          }
        })
        .catch((err) => {
          if (!cancelled) setRemoteRulesError(String(err));
        })
        .finally(() => {
          if (!cancelled) setRemoteRulesLoading(false);
        });
    }, 180);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [filter, remotePageIndex, remoteParsed, viewSet?.id, viewSet?.remote?.local_path]);

  const targetOpts: { value: RuleTarget; label: string }[] = useMemo(
    () => [
      { value: "proxy", label: t("rules.targetProxy") },
      { value: "direct", label: t("rules.targetDirect") },
      { value: "block", label: t("rules.targetBlock") },
      { value: "node", label: t("rules.targetNode") },
      { value: "smart", label: t("rules.targetSmart") },
      { value: "chain", label: t("rules.targetChain") },
    ],
    [t],
  );

  const typeOpts: { value: RuleType; label: string }[] = useMemo(
    () => [
      { value: "domain_suffix", label: t("rules.typeDomainSuffix") },
      { value: "domain", label: t("rules.typeDomain") },
      { value: "domain_keyword", label: t("rules.typeDomainKeyword") },
      { value: "ip_cidr", label: t("rules.typeIpCidr") },
      { value: "process", label: t("rules.typeProcess") },
    ],
    [t],
  );

  /** Localized rule-type label; unknown kinds (e.g. from remote files)
   *  fall back to the raw value. */
  function ruleTypeLabel(type: string): string {
    return type === "domain_suffix"
      ? t("rules.typeDomainSuffix")
      : type === "domain"
        ? t("rules.typeDomain")
        : type === "domain_keyword"
          ? t("rules.typeDomainKeyword")
          : type === "ip_cidr"
            ? t("rules.typeIpCidr")
            : type === "process"
              ? t("rules.typeProcess")
              : type;
  }

  /** Localized set-strategy / DNS-strategy labels (no raw English in UI). */
  function strategyLabel(s: string): string {
    return s === "proxy"
      ? t("rules.targetProxy")
      : s === "direct"
        ? t("rules.targetDirect")
        : s === "block"
          ? t("rules.targetBlock")
          : s === "node"
            ? t("rules.strategyNode")
            : s === "filter"
              ? t("rules.strategyFilter")
              : s === "chain"
                ? t("rules.targetChain")
                : t("rules.strategySmart");
  }

  function dnsStrategyLabel(s: string): string {
    return s === "local"
      ? t("dns.finalLocal")
      : s === "domestic"
        ? t("dns.finalDomestic")
        : t("dns.finalRemote");
  }

  /** Per-rule target valid under the current set: advanced sets (mixed /
   *  node / filter) allow the advanced targets — the whole-set pin or
   *  keywords fill in any per-rule blanks at build time; plain sets are
   *  limited to proxy/direct/block. */
  function clampTargetForSet(target: RuleTarget): RuleTarget {
    if (viewSet?.strategy === "smart" || viewSet?.strategy === "node" || viewSet?.strategy === "filter") {
      return target;
    }
    return target === "proxy" || target === "direct" || target === "block"
      ? target
      : ((viewSet?.strategy ?? "proxy") as RuleTarget);
  }

  /** Per-rule editor options under the current set: mixed sets unlock every
   *  target; node / filter sets add their own follow-the-set target to the
   *  plain trio; plain sets stay at proxy/direct/block. */
  const editorTargetOpts: { value: RuleTarget; label: string }[] = useMemo(() => {
    const base = targetOpts.slice(0, 3);
    const s = viewSet?.strategy;
    if (s === "smart") return targetOpts;
    if (s === "node") return [...base, targetOpts[3]];
    if (s === "filter") return [...base, targetOpts[4]];
    return base;
  }, [viewSet?.strategy, targetOpts]);

  function targetLabel(r: Rule): { text: string; stale: boolean; cls: string } {
    if (r.target === "smart") {
      const parts: string[] = [t("rules.smartLabel")];
      const inc = (r.smart_include ?? []).filter(Boolean);
      const exc = (r.smart_exclude ?? []).filter(Boolean);
      if (inc.length) {
        parts.push(t("rules.smartLabelInc", { k: inc.join("/") }));
      }
      if (exc.length) {
        parts.push(t("rules.smartLabelExc", { k: exc.join("/") }));
      }
      return { text: parts.join(" · "), stale: false, cls: "target-smart" };
    }
    if (r.target === "chain") {
      const id = r.chain_id ?? "";
      const live = id ? chainById.get(id) : undefined;
      if (live) {
        return { text: live.name, stale: false, cls: "target-chain" };
      }
      const was = r.chain_name?.trim() || id || "—";
      return {
        text: t("rules.chainStaleLabel", { name: was }),
        stale: true,
        cls: "target-stale",
      };
    }
    if (r.target !== "node") {
      const text =
        r.target === "proxy"
          ? t("rules.targetProxy")
          : r.target === "direct"
            ? t("rules.targetDirect")
            : r.target === "block"
              ? t("rules.targetBlock")
              : r.target;
      return { text, stale: false, cls: `target-${r.target}` };
    }
    const id = r.node_id ?? "";
    const live = id ? nodeById.get(id) : undefined;
    if (live) {
      return { text: live.name, stale: false, cls: "target-node" };
    }
    const was = r.node_name?.trim() || id || "—";
    return {
      text: t("rules.nodeStaleLabel", { name: was }),
      stale: true,
      cls: "target-stale",
    };
  }

  async function ensureNodesLoaded() {
    try {
      const list = await listAllNodes();
      setNodes(list);
    } catch {
      setNodes([]);
    }
  }

  async function ensureChainsLoaded() {
    try {
      const list = await listChains();
      setChains(list);
    } catch {
      setChains([]);
    }
  }

  function openCreate() {
    setEditRule(null);
    setRuleType("domain_suffix");
    setPayload("");
    // Uniform sets start at their own target (node sets prefill the whole-set
    // pin; filter-set keywords stay empty and inherit the set filters).
    setTarget(setUniformTarget ?? "proxy");
    setPinNodeId(viewSet?.strategy === "node" ? (viewSet.node_id ?? "") : "");
    setNodeQuery("");
    setSmartInclude("");
    setSmartExclude("");
    setChainId("");
    setEnabled(true);
    setEditOpen(true);
    void ensureNodesLoaded();
    void ensureChainsLoaded();
  }

  function openEdit(r: Rule) {
    setEditRule(r);
    setRuleType(r.type);
    setPayload(r.payload);
    setTarget(clampTargetForSet(r.target));
    setPinNodeId(r.node_id ?? "");
    setNodeQuery("");
    setSmartInclude((r.smart_include ?? []).join(" "));
    setSmartExclude((r.smart_exclude ?? []).join(" "));
    setChainId(r.chain_id ?? "");
    setEnabled(r.enabled);
    setEditOpen(true);
    void ensureNodesLoaded();
    void ensureChainsLoaded();
  }

  async function onSave(e: FormEvent) {
    e.preventDefault();
    if (!viewSetId || !payload.trim() || plainDiverged) return;
    // Smart sets honor the full target list; plain sets honor the per-rule
    // proxy/direct/block choice (the builder routes each rule separately).
    const effectiveTarget = clampTargetForSet(target);
    if (effectiveTarget === "node" && !pinNodeId.trim()) {
      setError(t("rules.needNode"));
      return;
    }
    if (effectiveTarget === "smart" && smartKeywordOverlap.length > 0) {
      setError(
        t("rules.smartKeywordConflict", { k: smartKeywordOverlap.join("、") }),
      );
      return;
    }
    if (effectiveTarget === "chain" && !chainId.trim()) {
      setError(t("rules.needChain"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await saveRule({
        setId: viewSetId,
        id: editRule?.id ?? null,
        ruleType,
        payload: payload.trim(),
        target: effectiveTarget,
        ord: editRule?.ord ?? null,
        enabled,
        nodeId: effectiveTarget === "node" ? pinNodeId : null,
        smartInclude: effectiveTarget === "smart" ? parseKeywords(smartInclude) : null,
        smartExclude: effectiveTarget === "smart" ? parseKeywords(smartExclude) : null,
        chainId: effectiveTarget === "chain" ? chainId : null,
      });
      setEditOpen(false);
      await reloadRules(viewSetId);
      await reloadSets();
      void ensureNodesLoaded();
    } catch (err) {
      setError(typeof err === "string" ? err : String(err));
    } finally {
      setBusy(false);
    }
  }

  function nextToggleGen(id: string) {
    const g = (toggleGenRef.current.get(id) ?? 0) + 1;
    toggleGenRef.current.set(id, g);
    return g;
  }

  async function onToggleSet(id: string, nextEnabled: boolean) {
    const current = sets.find((s) => s.id === id);
    if (!current || current.enabled === nextEnabled) return;
    // Empty sets cannot be enabled (the backend rejects it too).
    if (nextEnabled && current.rule_count === 0) return;

    const prevEnabled = current.enabled;
    const gen = nextToggleGen(id);
    togglePrevRef.current.set(id, prevEnabled);

    // Optimistic: flip the switch immediately, restart happens in the
    // background (see the `rule-set-apply-status` listener below).
    setSets((list) =>
      list.map((s) => (s.id === id ? { ...s, enabled: nextEnabled } : s)),
    );
    setTogglingIds((cur) => new Set(cur).add(id));
    setError(null);

    try {
      await setRuleSetEnabled(id, nextEnabled); // resolves once persisted, not once restarted
    } catch (err) {
      // Only the latest click for this id should roll back / clear pending.
      if (toggleGenRef.current.get(id) === gen) {
        setSets((list) =>
          list.map((s) => (s.id === id ? { ...s, enabled: prevEnabled } : s)),
        );
        setTogglingIds((cur) => {
          const next = new Set(cur);
          next.delete(id);
          return next;
        });
        setError(typeof err === "string" ? err : String(err));
      }
    }
  }

  async function onDnsStrategyChange(strategy: RuleSetDnsStrategy) {
    if (!viewSetId || !viewSet || strategy === viewSet.dns_strategy || busy) return;
    setBusy(true);
    setError(null);
    try {
      await setRuleSetDnsStrategy(viewSetId, strategy);
      await Promise.all([reloadSets(), reloadRules(viewSetId)]);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  /** Convert a plain set to Mixed from the rule editor. Flip-to-smart keeps
   *  every rule's current target (the uniform base), so only the rule being
   *  edited ends up diverging; the editor stays open with all per-rule
   *  targets unlocked. */
  async function onConvertToSmart() {
    if (!viewSetId || busy) return;
    setBusy(true);
    setError(null);
    try {
      await setRuleSetStrategy(viewSetId, "smart");
      await Promise.all([reloadSets(), reloadRules(viewSetId)]);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  function openBatch() {
    setBatchTarget("proxy");
    setBatchNodeId("");
    setBatchNodeQuery("");
    setBatchSmartInclude("");
    setBatchSmartExclude("");
    setBatchChainId("");
    setBatchOpen(true);
    void ensureNodesLoaded();
    void ensureChainsLoaded();
  }

  async function onBatchApply(e: FormEvent) {
    e.preventDefault();
    if (!viewSetId || batchBusy) return;
    if (batchTarget === "node" && !batchNodeId.trim()) {
      setError(t("rules.needNode"));
      return;
    }
    if (batchTarget === "smart" && batchKeywordOverlap.length > 0) {
      setError(
        t("rules.smartKeywordConflict", { k: batchKeywordOverlap.join("、") }),
      );
      return;
    }
    if (batchTarget === "chain" && !batchChainId.trim()) {
      setError(t("rules.needChain"));
      return;
    }
    setBatchBusy(true);
    setError(null);
    try {
      await batchSetRuleTargets(
        viewSetId,
        batchTarget,
        batchTarget === "node" ? batchNodeId : null,
        batchTarget === "smart" ? parseKeywords(batchSmartInclude) : null,
        batchTarget === "smart" ? parseKeywords(batchSmartExclude) : null,
        batchTarget === "chain" ? batchChainId : null,
      );
      setBatchOpen(false);
      await Promise.all([reloadSets(), reloadRules(viewSetId)]);
    } catch (err) {
      setError(typeof err === "string" ? err : String(err));
    } finally {
      setBatchBusy(false);
    }
  }

  async function onResetAllBuiltin() {
    if (!confirm(t("rules.resetAllBuiltinConfirm"))) return;
    setBusy(true);
    setError(null);
    try {
      await resetBuiltinRuleSet();
      setViewSetId(null);
      await reload();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  }

  function openNewSet() {
    setNewSetName(t("rules.setNamePh"));
    setNewSetKind("local");
    setNewSetUrl("");
    setNewSetTarget("proxy");
    setNewSetNodeId("");
    setNewSetNodeQuery("");
    setNewSetSmartInclude("");
    setNewSetSmartExclude("");
    setNewSetChainId("");
    setNewSetUpdateInterval("disabled");
    setNewSetOpen(true);
    setError(null);
    void ensureNodesLoaded();
    void ensureChainsLoaded();
  }

  async function onCreateSet(e: FormEvent) {
    e.preventDefault();
    const name = newSetName.trim();
    if (!name) {
      setError(t("rules.needName"));
      return;
    }
    if (newSetTarget === "node" && !newSetNodeId.trim()) {
      setError(t("rules.needNode"));
      return;
    }
    if (newSetTarget === "chain" && !newSetChainId.trim()) {
      setError(t("rules.needChain"));
      return;
    }
    if (newSetKeywordOverlap.length > 0) {
      setError(
        t("rules.smartKeywordConflict", { k: newSetKeywordOverlap.join("、") }),
      );
      return;
    }
    setNewSetBusy(true);
    setError(null);
    try {
      if (newSetKind === "remote" && !/^https?:\/\//i.test(newSetUrl.trim())) {
        setError(t("rules.remoteUrlInvalid"));
        return;
      }
      const set = await createRuleSet(
        name,
        newSetKind === "remote" ? newSetUrl.trim() : null,
        // "filter" rides on the smart keyword-pool target; the backend maps
        // it to the whole-set Filter strategy.
        newSetTarget === "filter" ? "smart" : newSetTarget,
        newSetKind === "remote" ? newSetUpdateInterval : null,
        newSetTarget === "node" ? newSetNodeId : null,
        newSetTarget === "filter" ? parseKeywords(newSetSmartInclude) : null,
        newSetTarget === "filter" ? parseKeywords(newSetSmartExclude) : null,
        newSetTarget === "chain" ? newSetChainId : null,
      );
      const list = await listRuleSets();
      setSets(list);
      setViewSetId(set.id);
      setRules([]);
      setNewSetOpen(false);
      if (newSetKind === "remote") void onRefreshRemoteSet(set.id);
    } catch (err) {
      setError(typeof err === "string" ? err : String(err));
    } finally {
      setNewSetBusy(false);
    }
  }

  async function onRefreshRemoteSet(id: string) {
    setRemoteBusyIds((current) => new Set(current).add(id));
    setError(null);
    try {
      await refreshRemoteRuleSet(id);
      await reloadSets();
    } catch (err) {
      setError(typeof err === "string" ? err : String(err));
      await reloadSets().catch(() => undefined);
    } finally {
      setRemoteBusyIds((current) => {
        const next = new Set(current);
        next.delete(id);
        return next;
      });
    }
  }

  /** Xray mode: the builtin sets are backed by geodata files — refresh
   *  re-downloads the .dat files instead of the .srs cache. */
  async function onRefreshGeodata(setId: string) {
    setRemoteBusyIds((current) => new Set(current).add(setId));
    setError(null);
    try {
      setGeodata(await refreshGeodata(true, geoCore));
    } catch (err) {
      setError(typeof err === "string" ? err : String(err));
    } finally {
      setRemoteBusyIds((current) => {
        const next = new Set(current);
        next.delete(setId);
        return next;
      });
    }
  }

  function openEditSet(target: RuleSetSummary) {
    setMenuSetId(null);
    setEditSetTarget(target);
    setEditSetName(target.name);
    setEditSetUrl(target.remote?.url ?? "");
    const interval = target.remote?.update_interval;
    setEditSetUpdateInterval(
      interval === "1h" || interval === "12h" || interval === "24h"
        ? interval
        : "disabled",
    );
  }

  async function onEditSet(e: FormEvent) {
    e.preventDefault();
    if (!editSetTarget || !editSetName.trim() || editSetBusy) return;
    const id = editSetTarget.id;
    const remote = editSetTarget.remote;
    const nextUrl = editSetUrl.trim();
    const urlChanged = !!remote && remote.url !== nextUrl;
    setEditSetBusy(true);
    setError(null);
    try {
      await updateRuleSet(
        id,
        editSetName.trim(),
        remote ? nextUrl : null,
        remote ? editSetUpdateInterval : null,
      );
      await reloadSets();
      setEditSetTarget(null);
      if (urlChanged) void onRefreshRemoteSet(id);
    } catch (err) {
      setError(typeof err === "string" ? err : String(err));
    } finally {
      setEditSetBusy(false);
    }
  }

  async function onDeleteSet(target: RuleSetSummary | null | undefined = viewSet) {
    if (!target || busy) return;
    if (!confirm(t("rules.deleteSetConfirm", { name: target.name }))) return;
    setBusy(true);
    setError(null);
    try {
      await deleteRuleSet(target.id);
      if (viewSetId === target.id) setViewSetId(null);
      await reload();
    } catch (err) {
      setError(typeof err === "string" ? err : String(err));
    } finally {
      setBusy(false);
    }
  }

  function isFactorySet(s: RuleSetSummary | undefined | null) {
    if (!s) return false;
    // Only the bundled remote rule sets are resettable; legacy `builtin-*`
    // list sets are recognized but no longer restorable.
    return s.resettable;
  }

  async function onResetFactory(target: RuleSetSummary | null | undefined = viewSet) {
    if (!target || !isFactorySet(target)) return;
    const name = target.name;
    if (
      !confirm(
        t("rules.resetSingleConfirm", { name }),
      )
    ) {
      return;
    }
    try {
      await resetRuleSet(target.id);
      if (viewSetId === target.id) await reloadRules(target.id);
      await reloadSets();
    } catch (err) {
      setError(typeof err === "string" ? err : String(err));
    }
  }

  async function onToggle(rule: Rule) {
    if (!viewSetId) return;
    try {
      await setRuleEnabled(rule.id, !rule.enabled, viewSetId);
      await reloadRules(viewSetId);
    } catch (err) {
      setError(typeof err === "string" ? err : String(err));
    }
  }

  async function onDelete(id: string) {
    if (!viewSetId || !confirm(t("rules.deleteRuleConfirm"))) return;
    try {
      await removeRule(id, viewSetId);
      await reloadRules(viewSetId);
      await reloadSets();
    } catch (err) {
      setError(typeof err === "string" ? err : String(err));
    }
  }

  async function persistOrder(items: RuleSetSummary[], startIds: string[]) {
    const orderedIds = items.map((s) => s.id);
    if (orderedIds.join("\0") === startIds.join("\0")) return;
    if (persistLockRef.current) return;
    persistLockRef.current = true;
    setError(null);
    try {
      const list = await reorderRuleSets(orderedIds);
      setSets(list);
    } catch (err) {
      setError(typeof err === "string" ? err : String(err));
      await reloadSets();
    } finally {
      persistLockRef.current = false;
    }
  }

  /**
   * Pointer-based card sorting (HTML5 DnD is unreliable in Tauri WebView):
   * lift a fixed clone out of the list, open a highlighted insertion gap,
   * then persist the committed order.
   */
  const { drag, onItemPointerDown } = useRulesetDragSort<RuleSetSummary>({
    items: sets,
    onReorder: (next, startIds) => {
      setSets(next);
      void persistOrder(next, startIds);
    },
  });

  async function moveSet(id: string, dir: -1 | 1) {
    const list = setsRef.current;
    const idx = list.findIndex((s) => s.id === id);
    const to = idx + dir;
    if (idx < 0 || to < 0 || to >= list.length) return;
    const next = [...list];
    const [moved] = next.splice(idx, 1);
    next.splice(to, 0, moved);
    setSets(next);
    await persistOrder(
      next,
      list.map((s) => s.id),
    );
  }

  // Sidebar cards. During a drag the source card leaves the list (a fixed
  // clone follows the pointer) and a highlighted gap opens at the live
  // insertion slot; numbering follows rendered slots so the pending order is
  // visible while dragging. Iterating the gap over the *others* sequence (the
  // source excluded) keeps exactly one gap per render — keying it over the
  // raw list duplicated the gap whenever insertIndex === fromIndex and left
  // orphaned gap nodes behind after the drag.
  const setCards: ReactNode[] = [];
  {
    const others = drag ? sets.filter((s) => s.id !== drag.id) : sets;
    for (let index = 0; index < others.length; index++) {
      const s = others[index];
      if (drag && setCards.length === drag.insertIndex) {
        setCards.push(
          <div
            key="ruleset-drop-gap"
            className="ruleset-drop-gap"
            style={{ height: drag.height }}
            aria-hidden="true"
          />,
        );
      }
      // User-added .srs sets are skipped by the Xray generator (builtin
      // remote sets map to geosite/geoip and keep working).
      const srsInert = !!(geoCore && s.remote && !s.builtin);
      setCards.push(
        <div
          key={s.id}
          data-ruleset-id={s.id}
          className={[
            "ruleset-item",
            viewSetId === s.id ? "selected" : "",
            srsInert ? "xray-inert" : "",
          ]
            .filter(Boolean)
            .join(" ")}
          style={srsInert ? { opacity: 0.55 } : undefined}
          title={srsInert ? t("rules.remoteSrsInert") : undefined}
          onPointerDown={(e) => onItemPointerDown(s.id, e)}
          onClick={() => setViewSetId(s.id)}
          role="listitem"
          tabIndex={0}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === " ") {
              setViewSetId(s.id);
            }
            if (e.key === "ArrowUp") {
              e.preventDefault();
              void moveSet(s.id, -1);
            }
            if (e.key === "ArrowDown") {
              e.preventDefault();
              void moveSet(s.id, 1);
            }
          }}
        >
          <div className="ruleset-item-top">
            <span
              className="ruleset-drag"
              title={t("rules.dragPrio")}
              aria-hidden="true"
            >
              ⋮⋮
            </span>
            <span className="ruleset-prio muted">{setCards.length + 1}</span>
            <span className="ruleset-name">{s.name}</span>
            {s.remote &&
              (remoteBusyIds.has(s.id) ||
                s.remote.download_status === "downloading") && (
                <span
                  className="lat-spinner ruleset-download-spinner"
                  title={t("rules.downloadingTooltip")}
                  aria-label={t("rules.downloadingTooltip")}
                />
              )}
            {togglingIds.has(s.id) && (
              <span
                className="lat-spinner ruleset-toggle-spinner"
                title={t("rules.applyingTooltip")}
                aria-label={t("rules.applyingTooltip")}
              />
            )}
            <GlassSwitchControl
              checked={s.enabled}
              size="sm"
              disabled={
                (!s.enabled && s.rule_count === 0) || srsInert
              }
              title={
                srsInert
                  ? t("rules.remoteSrsInert")
                  : !s.enabled && s.rule_count === 0
                    ? t("rules.enableEmptyHint")
                    : s.enabled
                      ? t("rules.disableSet")
                      : t("rules.enableSet")
              }
              onClick={(e) => {
                e.stopPropagation();
              }}
              onChange={(checked) => void onToggleSet(s.id, checked)}
            />
            <div className="rule-menu" data-ruleset-menu>
              <button
                type="button"
                className="rule-menu-trigger"
                aria-label={t("rules.setMenuAria", { name: s.name })}
                aria-haspopup="menu"
                aria-expanded={menuSetId === s.id}
                onClick={(e) => {
                  e.stopPropagation();
                  setMenuRuleId(null);
                  setMenuSetId((id) => (id === s.id ? null : s.id));
                }}
              >
                ⋮
              </button>
              {menuSetId === s.id && (
                <div
                  className={`rule-menu-pop ruleset-menu-pop${
                    index < Math.ceil(sets.length / 2) ? " open-down" : ""
                  }`}
                  role="menu"
                >
                  <button
                    type="button"
                    role="menuitem"
                    className="rule-menu-item"
                    onClick={(e) => {
                      e.stopPropagation();
                      openEditSet(s);
                    }}
                  >
                    {t("rules.editSet")}
                  </button>
                  {isFactorySet(s) && (
                    <button
                      type="button"
                      role="menuitem"
                      className="rule-menu-item"
                      onClick={(e) => {
                        e.stopPropagation();
                        setMenuSetId(null);
                        void onResetFactory(s);
                      }}
                    >
                      {t("rules.resetFactory")}
                    </button>
                  )}
                  {s.remote && (
                    <button
                      type="button"
                      role="menuitem"
                      className="rule-menu-item"
                      disabled={remoteBusyIds.has(s.id)}
                      onClick={(e) => {
                        e.stopPropagation();
                        setMenuSetId(null);
                        // Builtin sets under Xray refresh the geodata files.
                        if (geoCore && s.builtin && XRAY_GEODATA_SETS[s.id]) {
                          void onRefreshGeodata(s.id);
                        } else {
                          void onRefreshRemoteSet(s.id);
                        }
                      }}
                    >
                      {t("common.update")}
                    </button>
                  )}
                  <button
                    type="button"
                    role="menuitem"
                    className="rule-menu-item danger"
                    disabled={
                      !!s.remote &&
                      (remoteBusyIds.has(s.id) ||
                        s.remote.download_status === "downloading")
                    }
                    onClick={(e) => {
                      e.stopPropagation();
                      setMenuSetId(null);
                      void onDeleteSet(s);
                    }}
                  >
                    {t("common.delete")}
                  </button>
                </div>
              )}
            </div>
          </div>
          <div className="muted" style={{ fontSize: 12 }}>
            <span className={`ruleset-strategy-label strategy-${s.strategy}`}>
              {strategyLabel(s.strategy)}
            </span>
            {s.builtin && (
              <span className="ruleset-builtin-label">{t("rules.builtin")}</span>
            )}
            {" · "}
            {t("rules.rulesCount", { n: s.rule_count })}
          </div>
        </div>,
      );
    }
    if (drag && setCards.length === drag.insertIndex) {
      setCards.push(
        <div
          key="ruleset-drop-gap-end"
          className="ruleset-drop-gap"
          style={{ height: drag.height }}
          aria-hidden="true"
        />,
      );
    }
  }

  const body = (
    <>
      {!embedded && (
        <div className="rules-toolbar page-header">
          <div>
            <h1>{t("rules.title")}</h1>
            <p className="page-desc">{t("rules.desc")}</p>
          </div>
        </div>
      )}

      {error && (
        <ErrorModal message={error} onClose={() => setError(null)} />
      )}

      <div className="rules-layout">
        <aside className="card ruleset-list rules-route-list">
          <div className="ruleset-list-actions">
            <GlassButton
              icon="+"
              onClick={openNewSet}
              title={t("rules.newSetTitle")}
            >
              {t("rules.newSet")}
            </GlassButton>
            <GlassButton
              icon="↺"
              onClick={() => void onResetAllBuiltin()}
              disabled={busy}
              title={t("rules.resetAllBuiltinHint")}
            >
              {t("rules.resetAllBuiltin")}
            </GlassButton>
          </div>
          <div className="ruleset-final" title={t("rules.finalHint")}>
            <span className="muted ruleset-final-label">{t("rules.final")}</span>
            <GlassSeg
              value={routeFinal}
              ariaLabel={t("rules.final")}
              disabled={finalBusy}
              onChange={(v) => void onRouteFinalChange(v as RouteFinal)}
              options={[
                { value: "proxy", label: t("rules.finalProxy") },
                { value: "direct", label: t("rules.finalDirect") },
                { value: "block", label: t("rules.finalBlock") },
              ]}
            />
          </div>
          <div className="ruleset-list-title">
            {t("rules.sets")}
            <span className="ruleset-list-hint">{t("rules.dragHint")}</span>
          </div>
          {setCards}
        </aside>

        <section className="rules-main">
          <div className="rules-toolbar card">
            <div className="header-actions rules-main-actions">
              <div className="rules-policy-control">
                <span className="muted rules-policy-label">{t("rules.routeLabel")}</span>
                <span
                  className={`pill target-${viewSet?.strategy ?? "proxy"} rules-strategy-pill`}
                  title={t("rules.strategyEditHint")}
                  aria-label={t("rules.routeStrategyAria")}
                >
                  {strategyLabel(viewSet?.strategy ?? "proxy")}
                </span>
              </div>
              {viewSet && (
              <div className="rule-menu rules-more" data-toolbar-menu>
                <button
                  type="button"
                  className="rule-menu-trigger"
                  aria-label={t("rules.moreAria")}
                  aria-haspopup="menu"
                  aria-expanded={toolbarMenuOpen}
                  onClick={() => setToolbarMenuOpen((v) => !v)}
                >
                  ⋮
                </button>
                {toolbarMenuOpen && (
                  <div className="rule-menu-pop rules-more-pop" role="menu">
                    {viewSet.strategy !== "block" && (
                      <div className="rules-more-section">
                        <div className="muted rules-more-title">
                          {t("rules.setMenuDnsTitle")}
                        </div>
                        {(["local", "domestic", "remote"] as const).map((value) => {
                          const active =
                            (viewSet?.dns_strategy ?? "remote") === value;
                          return (
                            <button
                              key={value}
                              type="button"
                              role="menuitemradio"
                              aria-checked={active}
                              className="rule-menu-item rules-more-option"
                              disabled={!viewSet || busy}
                              onClick={() => void onDnsStrategyChange(value)}
                            >
                              <span
                                className={`rules-more-radio${active ? " on" : ""}`}
                                aria-hidden
                              />
                              {dnsStrategyLabel(value)}
                            </button>
                          );
                        })}
                      </div>
                    )}
                    <div className="rules-more-divider" />
                    <button
                      type="button"
                      role="menuitem"
                      className="rule-menu-item"
                      onClick={() => {
                        setToolbarMenuOpen(false);
                        openBatch();
                      }}
                    >
                      {t("rules.batchSetRules")}
                    </button>
                  </div>
                )}
              </div>
              )}
              <div className="rules-toolbar-tail">
                <input
                  autoCapitalize="off"
                  autoCorrect="off"
                  spellCheck={false}
                  className="search rules-filter"
                  placeholder={t("rules.filter")}
                  value={filter}
                  onChange={(e) => {
                    setFilter(e.target.value);
                    setRemotePageIndex(0);
                  }}
                />
                <GlassButton
                  icon="+"
                  onClick={openCreate}
                  disabled={!viewSetId || !!viewSet?.remote}
                  title={t("rules.addRuleTitle")}
                >
                  {t("common.add")}
                </GlassButton>
              </div>
            </div>
          </div>

          {loading ? (
            <div className="empty">{t("common.loading")}</div>
          ) : geoCore &&
              viewSet?.remote &&
              viewSet.builtin &&
              XRAY_GEODATA_SETS[viewSet.id] ? (
            (() => {
              const geo = XRAY_GEODATA_SETS[viewSet.id];
              const file =
                geo.dat === "geoip" ? geodata?.geoip : geodata?.geosite;
              const busy = remoteBusyIds.has(viewSet.id);
              return (
                <div className="card remote-rule-status">
                  <div className="muted">
                    {t("rules.xrayGeoHint", { matcher: geo.matcher })}
                  </div>
                  <div className="muted" style={{ marginTop: "0.3rem" }}>
                    {geoCore === "mihomo"
                      ? t("rules.mihomoGeoSource")
                      : t("rules.xrayGeoSource")}
                  </div>
                  <div className="remote-cache-row" style={{ marginTop: "0.45rem" }}>
                    <span className="muted">{t("rules.cacheFile")}</span>
                    <code className="remote-cache-path">
                      {geoCore === "mihomo"
                        ? geo.dat === "geoip"
                          ? "Country.mmdb"
                          : "GeoSite.dat"
                        : `${geo.dat}.dat`}
                      {file?.present
                        ? ` · ${fmtDatSize(file.bytes)} · ${fmtDatTime(file.modified_at)}`
                        : ""}
                    </code>
                    <GlassButton
                      onClick={() => void onRefreshGeodata(viewSet.id)}
                      disabled={busy}
                    >
                      {busy ? t("rules.xrayGeoUpdating") : t("rules.xrayGeoUpdate")}
                    </GlassButton>
                  </div>
                  {file && !file.present && (
                    <div className="muted" style={{ marginTop: "0.35rem" }}>
                      {t("rules.xrayGeoMissing")}
                    </div>
                  )}
                </div>
              );
            })()
          ) : viewSet?.remote ? (
            <>
              <div className="card remote-rule-status">
                <div className="muted">
                  {viewSet.remote.download_status === "downloading"
                    ? t("rules.remoteDownloadingHint")
                    : viewSet.remote.download_status === "error"
                      ? t("rules.remoteDownloadFailed", {
                          error: viewSet.remote.download_error ?? t("rules.unknownError"),
                        })
                      : viewSet.remote.local_path
                        ? remoteParsed
                          ? t("rules.remoteParsedHint", { n: viewSet.rule_count })
                          : t("rules.remoteCachedHint", {
                              n: viewSet.rule_count ?? viewSet.remote.rule_count ?? 0,
                            })
                        : t("rules.remoteWaitingHint")}
                </div>
                {viewSet.remote.local_path && (
                  <div className="remote-cache-row">
                    <span className="muted">{t("rules.cacheFile")}</span>
                    <code className="remote-cache-path" title={viewSet.remote.local_path}>
                      {viewSet.remote.local_path}
                    </code>
                    {!remoteParsed && (
                      <GlassButton
                        onClick={() => setRemoteParsed(true)}
                        disabled={remoteRulesLoading}
                      >
                        {remoteRulesLoading
                          ? t("rules.parsingRules")
                          : t("rules.parseAndShow")}
                      </GlassButton>
                    )}
                  </div>
                )}
              </div>
              {remoteRulesLoading && !remotePage ? (
                <div className="empty muted">{t("rules.parsingRules")}</div>
              ) : remoteRulesError ? (
                <div className="empty card error">{t("rules.parseFailed", { error: remoteRulesError })}</div>
              ) : remotePage && remotePage.items.length > 0 ? (
                <div className="card table-wrap remote-rules-wrap">
                  <table className="remote-rules-table">
                    <colgroup>
                      <col className="col-index" />
                      <col className="col-kind" />
                      <col />
                    </colgroup>
                    <thead>
                      <tr>
                        <th>#</th>
                        <th>{t("rules.type")}</th>
                        <th>{t("rules.matchCondition")}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {remotePage.items.map((item) => (
                        <tr key={item.index}>
                          <td className="rule-ord">{item.index}</td>
                          <td className="rule-type"><code>{ruleTypeLabel(item.kind)}</code></td>
                          <td>
                            {item.complex ? (
                              <details className="remote-rule-details">
                                <summary title={item.summary}>
                                  {item.summary || t("rules.viewRawRule")}
                                </summary>
                                <pre>{item.raw}</pre>
                                {item.raw_truncated && (
                                  <div className="muted remote-rule-truncated">
                                    {t("rules.truncatedHint")}
                                  </div>
                                )}
                              </details>
                            ) : (
                              <div className="remote-rule-summary" title={item.summary}>
                                {item.summary}
                              </div>
                            )}
                          </td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                  <div className="remote-rule-pagination">
                    <span className="muted">
                      {remotePage.offset + 1}–{remotePage.offset + remotePage.items.length} / {remotePage.total}
                    </span>
                    <GlassButton
                      onClick={() => setRemotePageIndex((page) => Math.max(0, page - 1))}
                      disabled={remotePageIndex === 0 || remoteRulesLoading}
                    >
                      {t("rules.prevPage")}
                    </GlassButton>
                    <GlassButton
                      onClick={() => setRemotePageIndex((page) => page + 1)}
                      disabled={remotePage.offset + remotePage.items.length >= remotePage.total || remoteRulesLoading}
                    >
                      {t("rules.nextPage")}
                    </GlassButton>
                  </div>
                </div>
              ) : remoteParsed && viewSet.remote.local_path ? (
                <div className="empty card muted">
                  {filter.trim() ? t("rules.noMatchRemote") : t("rules.emptyRemote")}
                </div>
              ) : null}
            </>
          ) : filtered.length === 0 ? (
            <div className="empty card muted">{t("rules.empty")}</div>
          ) : (
            <div className="card table-wrap rules-table-wrap">
              <table className="rules-table">
                <colgroup>
                  <col className="col-ord" />
                  <col className="col-type" />
                  <col className="col-payload" />
                  <col className="col-target" />
                  <col className="col-enabled" />
                  <col className="col-actions" />
                </colgroup>
                <thead>
                  <tr>
                    <th>{t("rules.ord")}</th>
                    <th>{t("rules.type")}</th>
                    <th>{t("rules.payload")}</th>
                    <th>{t("rules.target")}</th>
                    <th>{t("rules.enable")}</th>
                    <th></th>
                  </tr>
                </thead>
                <tbody>
                  {filtered.map((r) => (
                    <tr
                      key={r.id}
                      className={r.enabled ? "rule-row" : "rule-row row-disabled"}
                      onClick={() => openEdit(r)}
                    >
                      <td className="rule-ord">{r.ord}</td>
                      <td className="rule-type">
                        <code>{ruleTypeLabel(r.type)}</code>
                      </td>
                      <td className="rule-payload" title={r.payload}>
                        {r.payload}
                      </td>
                      <td className="rule-target">
                        {(() => {
                          const lab = targetLabel({
                            ...r,
                            target: clampTargetForSet(r.target),
                          });
                          return (
                            <span
                              className={`pill ${lab.cls}`}
                              title={
                                lab.stale
                                  ? t("rules.nodeStaleHint")
                                  : r.target === "node"
                                    ? r.node_id ?? lab.text
                                    : lab.text
                              }
                            >
                              {lab.text}
                            </span>
                          );
                        })()}
                      </td>
                      <td
                        className="rule-enabled-cell"
                        onClick={(e) => e.stopPropagation()}
                      >
                        <GlassSwitchControl
                          checked={r.enabled}
                          size="sm"
                          title={r.enabled ? t("rules.disableRule") : t("rules.enableRule")}
                          onChange={() => void onToggle(r)}
                        />
                      </td>
                      <td
                        className="rule-actions-cell"
                        onClick={(e) => e.stopPropagation()}
                      >
                        <div className="rule-menu" data-rule-menu>
                          <button
                            type="button"
                            className="rule-menu-trigger"
                            aria-label={t("rules.menuAria")}
                            aria-haspopup="menu"
                            aria-expanded={menuRuleId === r.id}
                            onClick={(e) => {
                              e.stopPropagation();
                              setMenuRuleId((id) =>
                                id === r.id ? null : r.id,
                              );
                            }}
                          >
                            ⋮
                          </button>
                          {menuRuleId === r.id && (
                            <div className="rule-menu-pop" role="menu">
                              <button
                                type="button"
                                role="menuitem"
                                className="rule-menu-item"
                                onClick={() => {
                                  setMenuRuleId(null);
                                  openEdit(r);
                                }}
                              >
                                {t("common.edit")}
                              </button>
                              <button
                                type="button"
                                role="menuitem"
                                className="rule-menu-item danger"
                                onClick={() => {
                                  setMenuRuleId(null);
                                  void onDelete(r.id);
                                }}
                              >
                                {t("common.delete")}
                              </button>
                            </div>
                          )}
                        </div>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </section>
      </div>

      {batchOpen && viewSet && (
        <div className="modal-backdrop">
          <div className="modal rules-form-modal">
            <header className="modal-header">
              <h2>{t("rules.batchTitle")}</h2>
              <button type="button" className="icon-btn" onClick={() => setBatchOpen(false)}>
                ×
              </button>
            </header>
            <form className="modal-body" onSubmit={(e) => void onBatchApply(e)}>
              <div className="muted" style={{ fontSize: 12 }}>
                {viewSet.remote
                  ? t("rules.batchRemoteHint", { name: viewSet.name })
                  : rules.length === 0
                    ? t("rules.batchEmpty")
                    : t("rules.batchHint", { name: viewSet.name, n: rules.length })}
              </div>
              <div className="field">
                <span>{t("rules.outbound")}</span>
                <SolidSelect
                  value={batchTarget}
                  options={targetOpts}
                  onChange={(v) => setBatchTarget(v as RuleTarget)}
                  aria-label={t("rules.outbound")}
                />
              </div>
              {batchTarget === "node" && (
                <div className="field rule-node-pick">
                  <span>{t("rules.pickNode")}</span>
                  {nodes.length === 0 ? (
                    <p className="muted" style={{ margin: 0, fontSize: 12 }}>
                      {t("rules.noNodes")}
                    </p>
                  ) : (
                    <>
                      <input
                        autoCapitalize="off"
                        autoCorrect="off"
                        spellCheck={false}
                        className="search"
                        value={batchNodeQuery}
                        onChange={(e) => setBatchNodeQuery(e.target.value)}
                        placeholder={t("rules.pickNodePh")}
                      />
                      <SolidSelect
                        list
                        listSize={Math.min(8, Math.max(4, batchFilteredNodes.length || 4))}
                        value={batchNodeId}
                        onChange={setBatchNodeId}
                        aria-label={t("rules.pickNode")}
                        options={[
                          { value: "", label: t("rules.needNode") },
                          ...batchFilteredNodes.map((n) => ({
                            value: n.id,
                            label: n.name,
                          })),
                        ]}
                      />
                    </>
                  )}
                </div>
              )}
              {batchTarget === "smart" && (
                <div className="field rule-smart-filters">
                  <label className="field" style={{ marginBottom: 8 }}>
                    <span>{t("rules.smartInclude")}</span>
                    <input
                      autoCapitalize="off"
                      autoCorrect="off"
                      spellCheck={false}
                      value={batchSmartInclude}
                      onChange={(e) => setBatchSmartInclude(e.target.value)}
                      placeholder={t("rules.smartIncludePh")}
                    />
                  </label>
                  <label className="field" style={{ marginBottom: 8 }}>
                    <span>{t("rules.smartExclude")}</span>
                    <input
                      autoCapitalize="off"
                      autoCorrect="off"
                      spellCheck={false}
                      value={batchSmartExclude}
                      onChange={(e) => setBatchSmartExclude(e.target.value)}
                      placeholder={t("rules.smartExcludePh")}
                    />
                  </label>
                  {batchKeywordOverlap.length > 0 ? (
                    <p className="banner error" style={{ margin: 0, fontSize: 12 }}>
                      {t("rules.smartKeywordConflict", {
                        k: batchKeywordOverlap.join("、"),
                      })}
                    </p>
                  ) : (
                    <p
                      className="muted"
                      style={{
                        margin: 0,
                        fontSize: 12,
                        color:
                          batchSmartMatchCount === 0
                            ? "var(--danger, #e55)"
                            : undefined,
                      }}
                    >
                      {t("rules.smartMatchCount", { n: batchSmartMatchCount })}
                    </p>
                  )}
                </div>
              )}
              {batchTarget === "chain" && (
                <div className="field rule-chain-pick">
                  <span>{t("rules.pickChain")}</span>
                  {chains.length === 0 ? (
                    <p className="muted" style={{ margin: 0, fontSize: 12 }}>
                      {t("rules.noChains")}
                    </p>
                  ) : (
                    <SolidSelect
                      value={batchChainId}
                      onChange={setBatchChainId}
                      aria-label={t("rules.pickChain")}
                      options={[
                        { value: "", label: t("rules.needChain") },
                        ...chains.map((c) => ({
                          value: c.id,
                          label: c.name,
                        })),
                      ]}
                    />
                  )}
                </div>
              )}
              <div className="muted" style={{ fontSize: 12 }}>
                {t("rules.batchStrategyPreview", {
                  s:
                    batchTarget === "node"
                      ? t("rules.strategyNode")
                      : batchTarget === "smart"
                        ? t("rules.strategyFilter")
                        : batchTarget === "chain"
                          ? t("rules.targetChain")
                          : strategyLabel(batchTarget),
                })}
              </div>
              <footer className="modal-footer">
                <GlassButton onClick={() => setBatchOpen(false)}>
                  {t("common.cancel")}
                </GlassButton>
                <GlassButton
                  variant="primary"
                  type="submit"
                  disabled={
                    batchBusy ||
                    (!viewSet.remote && rules.length === 0) ||
                    (batchTarget === "node" && !batchNodeId.trim()) ||
                    (batchTarget === "chain" && !batchChainId.trim())
                  }
                >
                  {batchBusy
                    ? t("common.loading")
                    : viewSet.remote
                      ? t("rules.batchApplySet")
                      : t("rules.batchApply", { n: rules.length })}
                </GlassButton>
              </footer>
            </form>
          </div>
        </div>
      )}

      {editOpen && (
        <div className="modal-backdrop">
          <div className="modal rules-form-modal">
            <header className="modal-header">
              <h2>{editRule ? t("rules.editRule") : t("rules.addRuleTitle")}</h2>
              <button type="button" className="icon-btn" onClick={() => setEditOpen(false)}>
                ×
              </button>
            </header>
            <form className="modal-body" onSubmit={(e) => void onSave(e)}>
              <div className="field">
                <span>{t("rules.type")}</span>
                <SolidSelect
                  value={ruleType}
                  options={typeOpts}
                  onChange={(v) => setRuleType(v as RuleType)}
                  aria-label={t("rules.type")}
                />
              </div>
              <label className="field">
                <span>{t("rules.matchContent")}</span>
                <input
                  autoCapitalize="off"
                  autoCorrect="off"
                  spellCheck={false}
                  value={payload}
                  onChange={(e) => setPayload(e.target.value)}
                  placeholder="google.com / youtube / 10.0.0.0/8"
                  autoFocus
                />
                {payloadSuggestion && (
                  <GlassButton
                    variant="primary"
                    className="payload-suggestion-btn"
                    title={t("rules.clickToReplace")}
                    onClick={() => setPayload(payloadSuggestion)}
                  >
                    {t("rules.urlDetectedHint")}{" "}
                    <span className="mono">{payloadSuggestion}</span>
                  </GlassButton>
                )}
              </label>
              <div className="field">
                <span>{t("rules.outbound")}</span>
                <SolidSelect
                  value={clampTargetForSet(target)}
                  options={editorTargetOpts}
                  onChange={(v) => setTarget(v as RuleTarget)}
                  aria-label={t("rules.outbound")}
                />
              </div>
              {plainDiverged && (
                <div className="banner guide rule-diverge-banner">
                  <span>{t("rules.plainDivergeBanner")}</span>
                  <GlassButton
                    onClick={() => void onConvertToSmart()}
                    disabled={busy}
                  >
                    {t("rules.convertToSmartCta")}
                  </GlassButton>
                </div>
              )}
              {viewSet?.strategy === "smart" && target === "node" && (
                <div className="field rule-node-pick">
                  <span>{t("rules.pickNode")}</span>
                  {nodes.length === 0 ? (
                    <p className="muted" style={{ margin: 0, fontSize: 12 }}>
                      {t("rules.noNodes")}
                    </p>
                  ) : (
                    <>
                      <input
                        autoCapitalize="off"
                        autoCorrect="off"
                        spellCheck={false}
                        className="search"
                        value={nodeQuery}
                        onChange={(e) => setNodeQuery(e.target.value)}
                        placeholder={t("rules.pickNodePh")}
                      />
                      <SolidSelect
                        list
                        listSize={Math.min(8, Math.max(4, filteredNodes.length || 4))}
                        value={pinNodeId}
                        onChange={setPinNodeId}
                        aria-label={t("rules.pickNode")}
                        options={[
                          { value: "", label: t("rules.needNode") },
                          ...(pinNodeId && !nodeById.has(pinNodeId)
                            ? [
                                {
                                  value: pinNodeId,
                                  label: t("rules.nodeStaleLabel", {
                                    name: editRule?.node_name ?? pinNodeId,
                                  }),
                                },
                              ]
                            : []),
                          ...filteredNodes.map((n) => ({
                            value: n.id,
                            label: n.name,
                          })),
                        ]}
                      />
                      {pinNodeId && !nodeById.has(pinNodeId) && (
                        <p className="banner error" style={{ margin: "8px 0 0" }}>
                          {t("rules.nodeStaleHint")}
                        </p>
                      )}
                    </>
                  )}
                </div>
              )}
              {viewSet?.strategy === "smart" && target === "smart" && (
                <div className="field rule-smart-filters">
                  <p className="muted" style={{ margin: "0 0 8px", fontSize: 12 }}>
                    {t("rules.smartHint")}
                  </p>
                  <label className="field" style={{ marginBottom: 8 }}>
                    <span>{t("rules.smartInclude")}</span>
                    <input
                      autoCapitalize="off"
                      autoCorrect="off"
                      spellCheck={false}
                      value={smartInclude}
                      onChange={(e) => setSmartInclude(e.target.value)}
                      placeholder={t("rules.smartIncludePh")}
                    />
                  </label>
                  <label className="field" style={{ marginBottom: 8 }}>
                    <span>{t("rules.smartExclude")}</span>
                    <input
                      autoCapitalize="off"
                      autoCorrect="off"
                      spellCheck={false}
                      value={smartExclude}
                      onChange={(e) => setSmartExclude(e.target.value)}
                      placeholder={t("rules.smartExcludePh")}
                    />
                  </label>
                  {smartKeywordOverlap.length > 0 ? (
                    <p
                      className="banner error"
                      style={{ margin: "0 0 6px", fontSize: 12 }}
                    >
                      {t("rules.smartKeywordConflict", {
                        k: smartKeywordOverlap.join("、"),
                      })}
                    </p>
                  ) : (
                    <p
                      className="muted"
                      style={{
                        margin: 0,
                        fontSize: 12,
                        color:
                          smartMatchCount === 0
                            ? "var(--danger, #e55)"
                            : undefined,
                      }}
                    >
                      {t("rules.smartMatchCount", { n: smartMatchCount })}
                    </p>
                  )}
                </div>
              )}
              {viewSet?.strategy === "smart" && target === "chain" && (
                <div className="field rule-chain-pick">
                  <span>{t("rules.pickChain")}</span>
                  {chains.length === 0 ? (
                    <p className="muted" style={{ margin: 0, fontSize: 12 }}>
                      {t("rules.noChains")}
                    </p>
                  ) : (
                    <SolidSelect
                      value={chainId}
                      onChange={setChainId}
                      aria-label={t("rules.pickChain")}
                      options={[
                        { value: "", label: t("rules.needChain") },
                        ...(chainId && !chainById.has(chainId)
                          ? [
                              {
                                value: chainId,
                                label: t("rules.chainStaleLabel", {
                                  name: editRule?.chain_name ?? chainId,
                                }),
                              },
                            ]
                          : []),
                        ...chains.map((c) => ({
                          value: c.id,
                          label: c.name,
                        })),
                      ]}
                    />
                  )}
                  {chainId && !chainById.has(chainId) && (
                    <p className="banner error" style={{ margin: "8px 0 0" }}>
                      {t("rules.chainStaleHint")}
                    </p>
                  )}
                </div>
              )}
              {viewSet?.strategy === "filter" && target === "smart" && (
                <p className="muted" style={{ margin: "0 0 8px", fontSize: 12 }}>
                  {t("rules.editorFilterInheritHint", {
                    k: [
                      ...(viewSet.smart_include ?? []),
                      ...(viewSet.smart_exclude ?? []).map((x) => `-${x}`),
                    ].join(" ") || t("rules.filterAllNodes"),
                  })}
                </p>
              )}
              <label className="sys-proxy-row" style={{ border: "none", paddingTop: 0, marginTop: 0 }}>
                <span>{t("rules.enabled")}</span>
                <GlassSwitchControl
                  checked={enabled}
                  title={t("rules.enabled")}
                  onChange={setEnabled}
                />
              </label>
              <footer className="modal-footer">
                <GlassButton onClick={() => setEditOpen(false)}>
                  {t("common.cancel")}
                </GlassButton>
                <GlassButton
                  type="submit"
                  variant="primary"
                  disabled={
                    busy ||
                    plainDiverged ||
                    !payload.trim() ||
                    (viewSet?.strategy === "smart" && target === "node" && !pinNodeId.trim()) ||
                    (viewSet?.strategy === "smart" && target === "smart" &&
                      (nodes.length === 0 || smartKeywordOverlap.length > 0))
                  }
                >
                  {busy ? t("common.saving") : t("common.save")}
                </GlassButton>
              </footer>
            </form>
          </div>
        </div>
      )}

      {newSetOpen && (
        <div
          className="modal-backdrop"
        >
          <div className="modal rules-form-modal">
            <header className="modal-header">
              <h2>{t("rules.newSetTitle")}</h2>
              <button
                type="button"
                className="icon-btn"
                onClick={() => setNewSetOpen(false)}
              >
                ×
              </button>
            </header>
            <form className="modal-body" onSubmit={(e) => void onCreateSet(e)}>
              <label className="field">
                <span>{t("rules.addModeLabel")}</span>
                <GlassSeg
                  value={newSetKind}
                  ariaLabel={t("rules.addModeLabel")}
                  onChange={(value) => setNewSetKind(value as "local" | "remote")}
                  options={[
                    { value: "local", label: t("rules.addModeLocal") },
                    { value: "remote", label: t("rules.addModeRemote") },
                  ]}
                />
              </label>
              <label className="field">
                <span>{t("rules.setName")}</span>
                <input
                  autoCapitalize="off"
                  autoCorrect="off"
                  spellCheck={false}
                  value={newSetName}
                  onChange={(e) => setNewSetName(e.target.value)}
                  placeholder={t("rules.setNamePhExample")}
                  autoFocus
                  maxLength={64}
                />
              </label>
              {newSetKind === "remote" && (
                <label className="field">
                  <span>{t("rules.addModeRemote")}</span>
                  <input
                    autoCapitalize="off"
                    autoCorrect="off"
                    spellCheck={false}
                    value={newSetUrl}
                    onChange={(e) => setNewSetUrl(e.target.value)}
                    placeholder="https://example.com/rules.json"
                  />
                </label>
              )}
              {/* Route choice for BOTH kinds — local and remote sets support
                  the same five whole-set strategies (Mixed stays an emergent
                  per-rule state, not a creation choice). */}
              <label className="field">
                <span>{t("rules.routeLabel")}</span>
                <GlassSeg
                  value={newSetTarget}
                  ariaLabel={t("rules.routeStrategyAria")}
                  onChange={(value) =>
                    setNewSetTarget(
                      value as "proxy" | "direct" | "block" | "node" | "filter" | "chain",
                    )
                  }
                  options={[
                    { value: "proxy", label: t("rules.targetProxy") },
                    { value: "direct", label: t("rules.targetDirect") },
                    { value: "block", label: t("rules.targetBlock") },
                    { value: "node", label: t("rules.strategyNode") },
                    { value: "filter", label: t("rules.strategyFilter") },
                    { value: "chain", label: t("rules.targetChain") },
                  ]}
                />
              </label>
              {newSetTarget === "node" && (
                <div className="field rule-node-pick">
                  <span>{t("rules.pickNode")}</span>
                  {nodes.length === 0 ? (
                    <p className="muted" style={{ margin: 0, fontSize: 12 }}>
                      {t("rules.noNodes")}
                    </p>
                  ) : (
                    <>
                      <input
                        autoCapitalize="off"
                        autoCorrect="off"
                        spellCheck={false}
                        className="search"
                        value={newSetNodeQuery}
                        onChange={(e) => setNewSetNodeQuery(e.target.value)}
                        placeholder={t("rules.pickNodePh")}
                      />
                      <SolidSelect
                        list
                        listSize={Math.min(
                          8,
                          Math.max(4, newSetFilteredNodes.length || 4),
                        )}
                        value={newSetNodeId}
                        onChange={setNewSetNodeId}
                        aria-label={t("rules.pickNode")}
                        options={[
                          { value: "", label: t("rules.needNode") },
                          ...newSetFilteredNodes.map((n) => ({
                            value: n.id,
                            label: n.name,
                          })),
                        ]}
                      />
                    </>
                  )}
                </div>
              )}
              {newSetTarget === "chain" && (
                <div className="field rule-chain-pick">
                  <span>{t("rules.pickChain")}</span>
                  {chains.length === 0 ? (
                    <p className="muted" style={{ margin: 0, fontSize: 12 }}>
                      {t("rules.noChains")}
                    </p>
                  ) : (
                    <SolidSelect
                      value={newSetChainId}
                      onChange={setNewSetChainId}
                      aria-label={t("rules.pickChain")}
                      options={[
                        { value: "", label: t("rules.needChain") },
                        ...chains.map((c) => ({
                          value: c.id,
                          label: c.name,
                        })),
                      ]}
                    />
                  )}
                </div>
              )}
              {newSetTarget === "filter" && (
                <div className="field rule-smart-filters">
                  <p className="muted" style={{ margin: "0 0 8px", fontSize: 12 }}>
                    {t("rules.createSetFilterHint")}
                  </p>
                  <label className="field" style={{ marginBottom: 8 }}>
                    <span>{t("rules.smartInclude")}</span>
                    <input
                      autoCapitalize="off"
                      autoCorrect="off"
                      spellCheck={false}
                      value={newSetSmartInclude}
                      onChange={(e) => setNewSetSmartInclude(e.target.value)}
                      placeholder={t("rules.smartIncludePh")}
                    />
                  </label>
                  <label className="field" style={{ marginBottom: 8 }}>
                    <span>{t("rules.smartExclude")}</span>
                    <input
                      autoCapitalize="off"
                      autoCorrect="off"
                      spellCheck={false}
                      value={newSetSmartExclude}
                      onChange={(e) => setNewSetSmartExclude(e.target.value)}
                      placeholder={t("rules.smartExcludePh")}
                    />
                  </label>
                  {newSetKeywordOverlap.length > 0 ? (
                    <p className="banner error" style={{ margin: 0, fontSize: 12 }}>
                      {t("rules.smartKeywordConflict", {
                        k: newSetKeywordOverlap.join("、"),
                      })}
                    </p>
                  ) : (
                    <p
                      className="muted"
                      style={{
                        margin: 0,
                        fontSize: 12,
                        color:
                          newSetSmartMatchCount === 0
                            ? "var(--danger, #e55)"
                            : undefined,
                      }}
                    >
                      {t("rules.smartMatchCount", { n: newSetSmartMatchCount })}
                    </p>
                  )}
                </div>
              )}
              {newSetTarget === "node" && (
                <p className="muted" style={{ fontSize: 12, margin: 0 }}>
                  {t("rules.createSetNodeHint")}
                </p>
              )}
              {newSetKind === "remote" && (
                <>
                  <label className="field">
                    <span>{t("rules.autoUpdate")}</span>
                    <GlassSeg
                      value={newSetUpdateInterval}
                      ariaLabel={t("rules.autoUpdate")}
                      onChange={(value) =>
                        setNewSetUpdateInterval(
                          value as "disabled" | "1h" | "12h" | "24h",
                        )
                      }
                      options={[
                        { value: "disabled", label: t("rules.autoUpdateDisabled") },
                        { value: "1h", label: t("rules.autoUpdate1h") },
                        { value: "12h", label: t("rules.autoUpdate12h") },
                        { value: "24h", label: t("rules.autoUpdate24h") },
                      ]}
                    />
                  </label>
                </>
              )}
              <p className="muted" style={{ fontSize: 12, margin: 0 }}>
                {newSetKind === "remote"
                  ? t("rules.newSetRemoteHint")
                  : t("rules.newSetLocalHint")}
              </p>
              <footer className="modal-footer">
                <GlassButton disabled={newSetBusy} onClick={() => setNewSetOpen(false)}>
                  {t("common.cancel")}
                </GlassButton>
                <GlassButton
                  type="submit"
                  variant="primary"
                  disabled={
                    newSetBusy ||
                    !newSetName.trim() ||
                    (newSetKind === "remote" && !newSetUrl.trim()) ||
                    (newSetTarget === "node" &&
                      (nodes.length === 0 || !newSetNodeId.trim())) ||
                    newSetKeywordOverlap.length > 0
                  }
                >
                  {newSetBusy ? t("rules.creating") : t("rules.create")}
                </GlassButton>
              </footer>
            </form>
          </div>
        </div>
      )}

      {editSetTarget && (
        <div
          className="modal-backdrop"
        >
          <div className="modal rules-form-modal">
            <header className="modal-header">
              <h2>{t("rules.editSetTitle")}</h2>
              <button
                type="button"
                className="icon-btn"
                disabled={editSetBusy}
                onClick={() => setEditSetTarget(null)}
              >
                ×
              </button>
            </header>
            <form className="modal-body" onSubmit={(e) => void onEditSet(e)}>
              <label className="field">
                <span>{t("rules.setName")}</span>
                <input
                  autoCapitalize="off"
                  autoCorrect="off"
                  spellCheck={false}
                  value={editSetName}
                  onChange={(e) => setEditSetName(e.target.value)}
                  autoFocus
                  maxLength={64}
                />
              </label>
              {editSetTarget.remote && (
                <>
                  <label className="field">
                    <span>{t("rules.addModeRemote")}</span>
                    <input
                      autoCapitalize="off"
                      autoCorrect="off"
                      spellCheck={false}
                      value={editSetUrl}
                      onChange={(e) => setEditSetUrl(e.target.value)}
                      placeholder="https://example.com/rules.json"
                      disabled={editSetTarget.resettable}
                    />
                    {editSetTarget.resettable && (
                      <span className="field-hint muted">
                        {t("rules.systemUrlLocked")}
                      </span>
                    )}
                  </label>
                  <label className="field">
                    <span>{t("rules.autoUpdate")}</span>
                    <GlassSeg
                      value={editSetUpdateInterval}
                      ariaLabel={t("rules.autoUpdate")}
                      onChange={(value) =>
                        setEditSetUpdateInterval(
                          value as "disabled" | "1h" | "12h" | "24h",
                        )
                      }
                      options={[
                        { value: "disabled", label: t("rules.autoUpdateDisabled") },
                        { value: "1h", label: t("rules.autoUpdate1h") },
                        { value: "12h", label: t("rules.autoUpdate12h") },
                        { value: "24h", label: t("rules.autoUpdate24h") },
                      ]}
                    />
                  </label>
                </>
              )}
              <footer className="modal-footer">
                <GlassButton disabled={editSetBusy} onClick={() => setEditSetTarget(null)}>
                  {t("common.cancel")}
                </GlassButton>
                <GlassButton
                  type="submit"
                  variant="primary"
                  disabled={
                    editSetBusy ||
                    !editSetName.trim() ||
                    (!!editSetTarget.remote && !editSetUrl.trim())
                  }
                >
                  {editSetBusy ? t("common.saving") : t("common.save")}
                </GlassButton>
              </footer>
            </form>
          </div>
        </div>
      )}
    </>
  );

  if (embedded) {
    return <div className="settings-embed rules-embed">{body}</div>;
  }
  return <div className="page">{body}</div>;
}
