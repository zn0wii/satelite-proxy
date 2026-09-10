import { fmtCoreBytes, useCoreDownloadState } from "../coreDownload";
import { useI18n } from "../i18n";
import { CoreMark } from "./CoreMark";

/**
 * Floating bottom-right progress toast for core download/restore. Always
 * shown whenever a download/restore is in progress — including while the
 * Settings page itself is open, alongside its inline progress bar — because
 * it reads the same global store (coreDownload.ts) that survives
 * SettingsPage unmounting when the user switches to another tab mid-download.
 */
export function CoreDownloadToast() {
  const { t } = useI18n();
  const state = useCoreDownloadState();
  if (!state) return null;
  const { kind, progress, error } = state;
  const name = kind === "xray" ? "Xray" : kind === "mihomo" ? "mihomo" : "sing-box";

  return (
    <div className="core-download-toast" role="status" aria-live="polite">
      <div className="core-download-toast-head">
        <span className="ver-mark kernel-mark" aria-hidden>
          <CoreMark kind={kind} />
        </span>
        <span className="core-download-toast-name">{name}</span>
        {!error && <span className="lat-spinner" aria-hidden />}
      </div>
      {error ? (
        <div className="core-download-toast-error">{error}</div>
      ) : progress ? (
        <>
          <div className="core-download-progress-head">
            <span>
              {progress.stage === "preparing"
                ? t("settings.corePreparing")
                : progress.stage === "installing"
                  ? t("settings.coreInstalling")
                  : progress.stage === "assets"
                    ? t("settings.coreAssets")
                    : t("settings.coreDownloading")}
            </span>
            <span className="mono core-download-percent">
              {progress.percent != null ? `${progress.percent}%` : "…"}
            </span>
          </div>
          <div
            className={`core-progress-track${progress.percent == null ? " indeterminate" : ""}`}
          >
            <span style={{ width: `${progress.percent ?? 24}%` }} />
          </div>
          {progress.downloaded > 0 && (
            <div className="muted mono core-download-bytes">
              {fmtCoreBytes(progress.downloaded)}
              {progress.total ? ` / ${fmtCoreBytes(progress.total)}` : ""}
            </div>
          )}
        </>
      ) : null}
    </div>
  );
}
