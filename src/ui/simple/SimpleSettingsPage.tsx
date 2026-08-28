import { useCallback, useEffect, useState } from "react";
import { getSettings, updateSettings } from "../../api";
import { GlassSeg } from "../../components/GlassSeg";
import { GlassSwitchControl } from "../../components/GlassSwitchControl";
import { ErrorModal } from "../../components/ErrorModal";
import { useI18n, type Locale } from "../../i18n";
import { useTheme } from "../../theme";
import type { AppSettings, HeroStyle, ThemeId } from "../../types";
import { useUiMode } from "../UiModeContext";

export function SimpleSettingsPage() {
  const { t, locale, setLocale } = useI18n();
  const { theme, setTheme, heroStyle, setHeroStyle } = useTheme();
  const { setMode } = useUiMode();
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [error, setError] = useState<string | null>(null);

  const reload = useCallback(async () => {
    try {
      setSettings(await getSettings());
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  async function patchSettings(partial: Parameters<typeof updateSettings>[0]) {
    setError(null);
    try {
      setSettings(await updateSettings(partial));
    } catch (e) {
      setError(typeof e === "string" ? e : String(e));
    }
  }

  const ready = !!settings;

  return (
    <div className="page simple-page simple-settings">
      <header className="page-header">
        <div>
          <h1>{t("settings.title")}</h1>
          <p className="page-desc">{t("simple.settingsHint")}</p>
        </div>
      </header>

      {error && (
        <ErrorModal message={error} onClose={() => setError(null)} />
      )}

      <section className="settings-panel" aria-label={t("settings.tabApp")}>
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
                onChange={(v) => void setLocale(v as Locale)}
                options={[
                  { value: "zh", label: t("settings.langZh") },
                  { value: "en", label: t("settings.langEn") },
                ]}
              />
            </div>
            <div className="settings-app-row">
              <div className="settings-app-text">
                <div className="settings-app-title">
                  {t("settings.launchAtLogin")}
                </div>
                <div className="settings-app-desc muted">
                  {t("settings.launchAtLoginDesc")}
                </div>
              </div>
              <GlassSwitchControl
                checked={!!settings?.launch_at_login}
                title={t("settings.launchAtLogin")}
                disabled={!ready}
                ready={ready}
                onChange={(next) =>
                  void patchSettings({ launchAtLogin: next })
                }
              />
            </div>
            <div className="settings-app-row">
              <div className="settings-app-text">
                <div className="settings-app-title">
                  {t("settings.autoStartProxy")}
                </div>
                <div className="settings-app-desc muted">
                  {t("settings.autoStartProxyDesc")}
                </div>
              </div>
              <GlassSwitchControl
                checked={!!settings?.auto_start_proxy}
                title={t("settings.autoStartProxy")}
                disabled={!ready}
                ready={ready}
                onChange={(next) =>
                  void patchSettings({ autoStartProxy: next })
                }
              />
            </div>
            <div className="settings-app-row">
              <div className="settings-app-text">
                <div className="settings-app-title">
                  {t("settings.closeToTray")}
                </div>
                <div className="settings-app-desc muted">
                  {t("settings.closeToTrayDesc")}
                </div>
              </div>
              <GlassSwitchControl
                checked={settings?.close_to_tray !== false}
                title={t("settings.closeToTray")}
                disabled={!ready}
                ready={ready}
                onChange={(next) => void patchSettings({ closeToTray: next })}
              />
            </div>
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
                onChange={(v) => void setTheme(v as ThemeId)}
                options={[
                  { value: "aerospace", label: t("settings.themeAerospace") },
                  { value: "day", label: t("settings.themeDay") },
                ]}
              />
            </div>
            <div className="settings-app-row settings-app-pref">
              <div className="settings-app-text">
                <div className="settings-app-title">
                  {t("settings.heroStyle")}
                </div>
                <div className="settings-app-desc muted">
                  {t("settings.heroStyleDesc")}
                </div>
              </div>
              <GlassSeg
                value={heroStyle}
                ariaLabel={t("settings.heroStyle")}
                onChange={(v) => void setHeroStyle(v as HeroStyle)}
                options={[
                  { value: "particle", label: t("settings.heroStyleParticle") },
                  { value: "radiance", label: t("settings.heroStyleRadiance") },
                  { value: "classic", label: t("settings.heroStyleClassic") },
                  { value: "smiley", label: t("settings.heroStyleSmiley") },
                ]}
              />
            </div>
          </div>
          </div>
          <button
            type="button"
            className="simple-link-row"
            onClick={() => setMode("pro")}
          >
            <div>
              <div className="settings-app-title">{t("simple.switchPro")}</div>
              <div className="settings-app-desc muted">
                {t("simple.switchProDesc")}
              </div>
            </div>
            <span className="muted">→</span>
          </button>
        </div>
      </section>
    </div>
  );
}
