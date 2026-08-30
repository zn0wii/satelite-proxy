/**
 * Node grouping for the nodes page (list + grid): by subscription, by
 * protocol, or by country. The country detector reads proxy-node names the
 * way they actually look in the wild — "🇭🇰香港高速01|BGP", "JP-Tokyo 01",
 * "美国 洛杉矶" — via four tiers, best tier wins:
 *
 *   1. flag emoji (regional-indicator pair → ISO code, works for any flag
 *      even when the country is not in the table below)
 *   2. Chinese keyword substring (香港 / 东京 / 迪拜 …)
 *   3. English alias word ("Hong Kong", "Tokyo" — latin tokens joined and
 *      matched whole-word)
 *   4. ISO tokens: alpha-2 must appear uppercase as a standalone latin token
 *      ("HK-01" → HK), alpha-3 case-insensitive ("JPN01" → JPN)
 *
 * Anything unmatched lands in the "other" group.
 */

import type { ProxyNode } from "./types";

export type GroupBy = "none" | "sub" | "proto" | "country";

export interface NodeGroup {
  /** Stable key (subscription id / protocol string / region id). */
  key: string;
  label: string;
  flag?: string;
  nodes: ProxyNode[];
}

interface Region {
  /** ISO 3166-1 alpha-2. */
  id: string;
  zh: string;
  en: string;
  flag: string;
  aliases?: string[];
}

// Ordered by rough likelihood; detection scans all regions per tier anyway,
// the order only breaks exact ties.
const REGIONS: Region[] = [
  { id: "HK", zh: "香港", en: "Hong Kong", flag: "🇭🇰", aliases: ["HKG", "hongkong"] },
  { id: "TW", zh: "台湾", en: "Taiwan", flag: "🇹🇼", aliases: ["台北", "新北", "桃园", "高雄", "TWN", "taipei", "taiwan"] },
  { id: "JP", zh: "日本", en: "Japan", flag: "🇯🇵", aliases: ["东京", "大阪", "埼玉", "名古屋", "JPN", "tokyo", "osaka", "japan"] },
  { id: "SG", zh: "新加坡", en: "Singapore", flag: "🇸🇬", aliases: ["狮城", "SGP", "singapore"] },
  { id: "US", zh: "美国", en: "United States", flag: "🇺🇸", aliases: ["洛杉矶", "圣何塞", "西雅图", "纽约", "芝加哥", "凤凰城", "达拉斯", "硅谷", "弗吉尼亚", "费利蒙", "圣克拉拉", "波特兰", "USA", "united states", "america", "los angeles", "san jose", "seattle", "new york", "dallas", "chicago", "phoenix"] },
  { id: "KR", zh: "韩国", en: "South Korea", flag: "🇰🇷", aliases: ["首尔", "春川", "KOR", "seoul", "korea"] },
  { id: "GB", zh: "英国", en: "United Kingdom", flag: "🇬🇧", aliases: ["伦敦", "英伦", "UK", "GBR", "britain", "england", "london", "united kingdom"] },
  { id: "DE", zh: "德国", en: "Germany", flag: "🇩🇪", aliases: ["法兰克福", "柏林", "DEU", "germany", "frankfurt", "berlin"] },
  { id: "FR", zh: "法国", en: "France", flag: "🇫🇷", aliases: ["巴黎", "FRA", "france", "paris"] },
  { id: "CA", zh: "加拿大", en: "Canada", flag: "🇨🇦", aliases: ["多伦多", "温哥华", "CAN", "canada", "toronto", "vancouver"] },
  { id: "AU", zh: "澳大利亚", en: "Australia", flag: "🇦🇺", aliases: ["悉尼", "墨尔本", "AUS", "australia", "sydney", "melbourne"] },
  { id: "NZ", zh: "新西兰", en: "New Zealand", flag: "🇳🇿", aliases: ["NZL", "new zealand"] },
  { id: "RU", zh: "俄罗斯", en: "Russia", flag: "🇷🇺", aliases: ["莫斯科", "圣彼得堡", "RUS", "russia", "moscow"] },
  { id: "IN", zh: "印度", en: "India", flag: "🇮🇳", aliases: ["孟买", "IND", "india", "mumbai"] },
  { id: "TR", zh: "土耳其", en: "Turkey", flag: "🇹🇷", aliases: ["伊斯坦布尔", "TUR", "turkey", "istanbul"] },
  { id: "MY", zh: "马来西亚", en: "Malaysia", flag: "🇲🇾", aliases: ["MYS", "malaysia"] },
  { id: "TH", zh: "泰国", en: "Thailand", flag: "🇹🇭", aliases: ["曼谷", "THA", "thailand", "bangkok"] },
  { id: "VN", zh: "越南", en: "Vietnam", flag: "🇻🇳", aliases: ["VNM", "vietnam"] },
  { id: "PH", zh: "菲律宾", en: "Philippines", flag: "🇵🇭", aliases: ["PHL", "philippines"] },
  { id: "ID", zh: "印度尼西亚", en: "Indonesia", flag: "🇮🇩", aliases: ["雅加达", "IDN", "indonesia", "jakarta"] },
  { id: "BR", zh: "巴西", en: "Brazil", flag: "🇧🇷", aliases: ["BRA", "brazil"] },
  { id: "AR", zh: "阿根廷", en: "Argentina", flag: "🇦🇷", aliases: ["ARG", "argentina"] },
  { id: "NL", zh: "荷兰", en: "Netherlands", flag: "🇳🇱", aliases: ["阿姆斯特丹", "NLD", "netherlands", "holland", "amsterdam"] },
  { id: "ES", zh: "西班牙", en: "Spain", flag: "🇪🇸", aliases: ["ESP", "spain"] },
  { id: "IT", zh: "意大利", en: "Italy", flag: "🇮🇹", aliases: ["米兰", "ITA", "italy", "milan"] },
  { id: "CH", zh: "瑞士", en: "Switzerland", flag: "🇨🇭", aliases: ["苏黎世", "CHE", "switzerland", "zurich"] },
  { id: "SE", zh: "瑞典", en: "Sweden", flag: "🇸🇪", aliases: ["SWE", "sweden", "stockholm"] },
  { id: "NO", zh: "挪威", en: "Norway", flag: "🇳🇴", aliases: ["NOR", "norway"] },
  { id: "FI", zh: "芬兰", en: "Finland", flag: "🇫🇮", aliases: ["FIN", "finland"] },
  { id: "DK", zh: "丹麦", en: "Denmark", flag: "🇩🇰", aliases: ["DNK", "denmark"] },
  { id: "PL", zh: "波兰", en: "Poland", flag: "🇵🇱", aliases: ["华沙", "POL", "poland", "warsaw"] },
  { id: "UA", zh: "乌克兰", en: "Ukraine", flag: "🇺🇦", aliases: ["UKR", "ukraine", "kyiv"] },
  { id: "AE", zh: "阿联酋", en: "UAE", flag: "🇦🇪", aliases: ["迪拜", "ARE", "uae", "emirates", "dubai"] },
  { id: "SA", zh: "沙特", en: "Saudi Arabia", flag: "🇸🇦", aliases: ["利雅得", "SAU", "saudi"] },
  { id: "IL", zh: "以色列", en: "Israel", flag: "🇮🇱", aliases: ["ISR", "israel"] },
  { id: "MX", zh: "墨西哥", en: "Mexico", flag: "🇲🇽", aliases: ["MEX", "mexico"] },
  { id: "CL", zh: "智利", en: "Chile", flag: "🇨🇱", aliases: ["CHL", "chile"] },
  { id: "ZA", zh: "南非", en: "South Africa", flag: "🇿🇦", aliases: ["ZAF", "south africa"] },
  { id: "EG", zh: "埃及", en: "Egypt", flag: "🇪🇬", aliases: ["开罗", "EGY", "egypt", "cairo"] },
  { id: "NG", zh: "尼日利亚", en: "Nigeria", flag: "🇳🇬", aliases: ["NGA", "nigeria"] },
  { id: "KE", zh: "肯尼亚", en: "Kenya", flag: "🇰🇪", aliases: ["KEN", "kenya"] },
  { id: "KZ", zh: "哈萨克斯坦", en: "Kazakhstan", flag: "🇰🇿", aliases: ["KAZ", "kazakhstan", "almaty"] },
  { id: "CN", zh: "中国", en: "China", flag: "🇨🇳", aliases: ["上海", "北京", "广州", "深圳", "CHN", "china", "shanghai", "beijing"] },
  { id: "MO", zh: "澳门", en: "Macau", flag: "🇲🇴", aliases: ["MAC", "macau", "macao"] },
  { id: "PA", zh: "巴拿马", en: "Panama", flag: "🇵🇦", aliases: ["PAN", "panama"] },
  { id: "AT", zh: "奥地利", en: "Austria", flag: "🇦🇹", aliases: ["维也纳", "AUT", "austria", "vienna"] },
  { id: "BE", zh: "比利时", en: "Belgium", flag: "🇧🇪", aliases: ["BEL", "belgium"] },
  { id: "PT", zh: "葡萄牙", en: "Portugal", flag: "🇵🇹", aliases: ["PRT", "portugal"] },
  { id: "CZ", zh: "捷克", en: "Czechia", flag: "🇨🇿", aliases: ["CZE", "czech"] },
  { id: "IE", zh: "爱尔兰", en: "Ireland", flag: "🇮🇪", aliases: ["IRL", "ireland"] },
  { id: "RO", zh: "罗马尼亚", en: "Romania", flag: "🇷🇴", aliases: ["ROU", "romania"] },
  { id: "BG", zh: "保加利亚", en: "Bulgaria", flag: "🇧🇬", aliases: ["BGR", "bulgaria"] },
  { id: "HU", zh: "匈牙利", en: "Hungary", flag: "🇭🇺", aliases: ["布达佩斯", "HUN", "hungary", "budapest"] },
  { id: "GR", zh: "希腊", en: "Greece", flag: "🇬🇷", aliases: ["GRC", "greece"] },
  { id: "LU", zh: "卢森堡", en: "Luxembourg", flag: "🇱🇺", aliases: ["LUX", "luxembourg"] },
  { id: "LA", zh: "老挝", en: "Laos", flag: "🇱🇦", aliases: ["LAO", "laos"] },
  { id: "KH", zh: "柬埔寨", en: "Cambodia", flag: "🇰🇭", aliases: ["KHM", "cambodia"] },
  { id: "MM", zh: "缅甸", en: "Myanmar", flag: "🇲🇲", aliases: ["MMR", "myanmar"] },
  { id: "PK", zh: "巴基斯坦", en: "Pakistan", flag: "🇵🇰", aliases: ["PAK", "pakistan"] },
  { id: "LK", zh: "斯里兰卡", en: "Sri Lanka", flag: "🇱🇰", aliases: ["LKA", "sri lanka"] },
  { id: "BD", zh: "孟加拉", en: "Bangladesh", flag: "🇧🇩", aliases: ["BGD", "bangladesh"] },
  { id: "NP", zh: "尼泊尔", en: "Nepal", flag: "🇳🇵", aliases: ["NPL", "nepal"] },
  { id: "MN", zh: "蒙古", en: "Mongolia", flag: "🇲🇳", aliases: ["MNG", "mongolia"] },
  { id: "GE", zh: "格鲁吉亚", en: "Georgia", flag: "🇬🇪", aliases: ["GEO", "georgia"] },
  { id: "AZ", zh: "阿塞拜疆", en: "Azerbaijan", flag: "🇦🇿", aliases: ["AZE", "azerbaijan"] },
];

interface Alias {
  /** 1 = Chinese substring, 2 = English word, 3 = ISO alpha-2, 4 = ISO alpha-3. */
  tier: number;
  value: string;
  region: Region;
}

const ALIASES: Alias[] = REGIONS.flatMap((region) => {
  const list: Alias[] = [
    { tier: 1, value: region.zh, region },
    { tier: 2, value: region.en, region },
    { tier: 3, value: region.id, region },
  ];
  for (const a of region.aliases ?? []) {
    const isLatin = /^[A-Za-z0-9 ]+$/.test(a);
    list.push({
      tier: isLatin ? (a.length === 2 ? 3 : a.length === 3 ? 4 : 2) : 1,
      value: a,
      region,
    });
  }
  return list.filter((a) => a.value.length > 0);
});

/** "🇭🇰" → "HK" (regional indicator pair → letters). */
function flagToIso(flag: string): string {
  let out = "";
  for (const ch of flag) {
    const code = ch.codePointAt(0) ?? 0;
    if (code >= 0x1f1e6 && code <= 0x1f1ff) {
      out += String.fromCharCode(0x41 + code - 0x1f1e6);
    }
  }
  return out;
}

function flagsOf(name: string): string[] {
  const m = name.match(/\p{Regional_Indicator}{2}/gu);
  return m ?? [];
}

function detectRegion(name: string): Region | null {
  // Tier 0 — a flag emoji is the strongest signal and covers countries not
  // in the table (synthesize a code-only region).
  for (const flag of flagsOf(name)) {
    const iso = flagToIso(flag);
    if (iso.length !== 2) continue;
    const known = REGIONS.find((r) => r.id === iso);
    if (known) return known;
    return { id: iso, zh: iso, en: iso, flag };
  }

  const words = name.match(/[A-Za-z0-9]+/g) ?? [];
  const latin = ` ${(words.map((w) => w.toLowerCase()).join(" "))} `;

  let best: { alias: Alias; len: number } | null = null;
  for (const alias of ALIASES) {
    let hit = false;
    if (alias.tier === 1) {
      hit = name.includes(alias.value);
    } else if (alias.tier === 2) {
      hit = latin.includes(` ${alias.value.toLowerCase()} `);
    } else if (alias.tier === 3) {
      // Alpha-2: exact uppercase token only ("HK-01" hits, "husk" doesn't).
      hit = words.some((w) => w === alias.value);
    } else {
      hit = words.some((w) => w.toLowerCase() === alias.value.toLowerCase());
    }
    if (hit && (!best || alias.tier < best.alias.tier || (alias.tier === best.alias.tier && alias.value.length > best.len))) {
      best = { alias, len: alias.value.length };
    }
  }
  return best ? best.alias.region : null;
}

export interface GroupLabels {
  other: string;
  noSub: string;
}

/** Group nodes by the chosen dimension; within-group order is preserved. */
export function groupNodes(
  nodes: ProxyNode[],
  by: GroupBy,
  locale: string,
  labels: GroupLabels,
): NodeGroup[] {
  if (by === "none") return [];

  if (by === "sub") {
    const map = new Map<string, NodeGroup>();
    for (const n of nodes) {
      const key = n.subscription_name ?? "__nosub";
      let g = map.get(key);
      if (!g) {
        g = {
          key,
          label: key === "__nosub" ? labels.noSub : key,
          nodes: [],
        };
        map.set(key, g);
      }
      g.nodes.push(n);
    }
    return sortGroups([...map.values()]);
  }

  if (by === "proto") {
    const map = new Map<string, NodeGroup>();
    for (const n of nodes) {
      let g = map.get(n.protocol);
      if (!g) {
        g = { key: n.protocol, label: n.protocol, nodes: [] };
        map.set(n.protocol, g);
      }
      g.nodes.push(n);
    }
    return sortGroups([...map.values()]);
  }

  // country
  const zh = locale !== "en";
  const map = new Map<string, NodeGroup>();
  for (const n of nodes) {
    const region = detectRegion(n.name);
    const key = region ? region.id : "__other";
    let g = map.get(key);
    if (!g) {
      g = region
        ? {
            key,
            label: zh ? region.zh : region.en,
            flag: region.flag,
            nodes: [],
          }
        : { key, label: labels.other, nodes: [] };
      map.set(key, g);
    }
    g.nodes.push(n);
  }
  return sortGroups([...map.values()]);
}

function sortGroups(groups: NodeGroup[]): NodeGroup[] {
  return groups.sort((a, b) => {
    const ao = a.key === "__other" || a.key === "__nosub" ? 1 : 0;
    const bo = b.key === "__other" || b.key === "__nosub" ? 1 : 0;
    if (ao !== bo) return ao - bo;
    return a.label.localeCompare(b.label, "zh-Hans-CN");
  });
}
