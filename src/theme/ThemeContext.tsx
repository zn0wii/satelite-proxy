import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import { getSettings, updateSettings } from "../api";
import type { HeroStyle, ThemeId } from "../types";
import { applyAccentToDom, applyGlowToDom, normalizeGlowId, resolveAccent } from "./accents";

const THEME_KEY = "satelite.theme";
const ACCENT_KEY = "satelite.accent";
const GLOW_KEY = "satelite.glow";
const HERO_KEY = "satelite.heroStyle";

export function normalizeTheme(raw: string | null | undefined): ThemeId {
  const t = (raw ?? "").trim().toLowerCase();
  if (t === "aerospace") return "aerospace";
  return "day";
}

export function readStoredTheme(): ThemeId {
  try {
    return normalizeTheme(localStorage.getItem(THEME_KEY));
  } catch {
    return "day";
  }
}

export function readStoredAccent(): string {
  try {
    return resolveAccent(localStorage.getItem(ACCENT_KEY)).id;
  } catch {
    return "green";
  }
}

export function readStoredGlow(): string {
  try {
    return normalizeGlowId(localStorage.getItem(GLOW_KEY));
  } catch {
    return "accent";
  }
}

function persistThemePref(theme: ThemeId, accent: string, glow: string) {
  try {
    localStorage.setItem(THEME_KEY, theme);
    localStorage.setItem(ACCENT_KEY, accent);
    localStorage.setItem(GLOW_KEY, glow);
  } catch {
    /* ignore */
  }
}

export function normalizeHeroStyle(raw: string | null | undefined): HeroStyle {
  const t = (raw ?? "").trim().toLowerCase();
  if (t === "classic") return "classic";
  if (t === "particle") return "particle";
  if (t === "radiance") return "radiance";
  return "smiley";
}

/** Mirror of the backend default (`default_hero_style`) — see readStoredHeroStyle. */
export function readStoredHeroStyle(): HeroStyle {
  try {
    return normalizeHeroStyle(localStorage.getItem(HERO_KEY));
  } catch {
    return "smiley";
  }
}

function persistHeroStylePref(style: HeroStyle) {
  try {
    localStorage.setItem(HERO_KEY, style);
  } catch {
    /* ignore */
  }
}

export function applyThemeToDom(theme: ThemeId, accent: string, glow: string) {
  document.documentElement.dataset.theme = theme;
  // Drive native <select> / form control chrome (WKWebView) with the UI theme.
  document.documentElement.style.colorScheme =
    theme === "day" ? "light" : "dark";
  applyAccentToDom(accent, theme);
  applyGlowToDom(glow, accent, theme);
  persistThemePref(theme, accent, glow);
}

/** Toggle the frosted-glass control look (see docs/webview2-memory-optimization-plan.md). */
export function applyGlassFrostToDom(frost: boolean) {
  if (frost) document.documentElement.dataset.glassFrost = "true";
  else delete document.documentElement.dataset.glassFrost;
}

interface ThemeContextValue {
  theme: ThemeId;
  setTheme: (next: ThemeId) => Promise<void>;
  accent: string;
  setAccent: (next: string) => Promise<void>;
  glow: string;
  setGlow: (next: string) => Promise<void>;
  heroStyle: HeroStyle;
  setHeroStyle: (next: HeroStyle) => Promise<void>;
  glassFrost: boolean;
  setGlassFrost: (next: boolean) => Promise<void>;
  ready: boolean;
}

const ThemeContext = createContext<ThemeContextValue | null>(null);

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [theme, setThemeState] = useState<ThemeId>(readStoredTheme);
  const [accent, setAccentState] = useState<string>(readStoredAccent);
  const [glow, setGlowState] = useState<string>(readStoredGlow);
  // Read from localStorage (mirrored on every change) so the FIRST render
  // already picks the right HeroVisual branch — otherwise a WebView recreate
  // would default to "particle" and needlessly pull the 530KB three.js chunk
  // until getSettings() lands.
  const [heroStyle, setHeroStyleState] = useState<HeroStyle>(readStoredHeroStyle);
  // Mirrors the backend default (default_glass_frost) to avoid a flash of
  // solid controls before settings land.
  const [glassFrost, setGlassFrostState] = useState(true);
  const [ready, setReady] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void getSettings()
      .then((s) => {
        if (cancelled) return;
        const nextTheme = normalizeTheme(s.theme);
        const nextAccent = resolveAccent(s.accent).id;
        const nextGlow = normalizeGlowId(s.glow_color);
        const nextHero = normalizeHeroStyle(s.hero_style);
        setThemeState(nextTheme);
        setAccentState(nextAccent);
        setGlowState(nextGlow);
        setHeroStyleState(nextHero);
        persistHeroStylePref(nextHero);
        setGlassFrostState(s.glass_frost === true);
        applyThemeToDom(nextTheme, nextAccent, nextGlow);
        applyGlassFrostToDom(s.glass_frost === true);
      })
      .catch(() => {
        applyThemeToDom(readStoredTheme(), readStoredAccent(), readStoredGlow());
      })
      .finally(() => {
        if (!cancelled) setReady(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const setTheme = useCallback(
    async (next: ThemeId) => {
      setThemeState(next);
      applyThemeToDom(next, accent, glow);
      try {
        await updateSettings({ theme: next });
      } catch {
        /* UI already switched */
      }
    },
    [accent, glow],
  );

  const setAccent = useCallback(
    async (next: string) => {
      const id = resolveAccent(next).id;
      setAccentState(id);
      applyThemeToDom(theme, id, glow);
      try {
        await updateSettings({ accent: id });
      } catch {
        /* UI already switched */
      }
    },
    [theme, glow],
  );

  const setGlow = useCallback(
    async (next: string) => {
      const id = normalizeGlowId(next);
      setGlowState(id);
      applyGlowToDom(id, accent, theme);
      try {
        localStorage.setItem(GLOW_KEY, id);
      } catch {
        /* ignore */
      }
      try {
        await updateSettings({ glowColor: id });
      } catch {
        /* UI already switched */
      }
    },
    [accent, theme],
  );

  const setHeroStyle = useCallback(async (next: HeroStyle) => {
    const style = normalizeHeroStyle(next);
    setHeroStyleState(style);
    persistHeroStylePref(style);
    try {
      await updateSettings({ heroStyle: style });
    } catch {
      /* UI already switched */
    }
  }, []);

  const setGlassFrost = useCallback(async (next: boolean) => {
    setGlassFrostState(next);
    applyGlassFrostToDom(next);
    try {
      await updateSettings({ glassFrost: next });
    } catch {
      /* UI already switched */
    }
  }, []);

  const value = useMemo(
    () => ({
      theme,
      setTheme,
      accent,
      setAccent,
      glow,
      setGlow,
      heroStyle,
      setHeroStyle,
      glassFrost,
      setGlassFrost,
      ready,
    }),
    [
      theme,
      setTheme,
      accent,
      setAccent,
      glow,
      setGlow,
      heroStyle,
      setHeroStyle,
      glassFrost,
      setGlassFrost,
      ready,
    ],
  );

  return (
    <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>
  );
}

export function useTheme(): ThemeContextValue {
  const ctx = useContext(ThemeContext);
  if (!ctx) {
    throw new Error("useTheme must be used within ThemeProvider");
  }
  return ctx;
}
