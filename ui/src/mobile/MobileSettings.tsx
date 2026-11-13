import { ChevronRight, Download, FileClock } from "lucide-react";
import { useAppInfoStore, useIsAndroid } from "../stores/appInfo";
import { type ThemePreference, useSettingsStore } from "../stores/settings";

const THEMES: readonly { id: ThemePreference; label: string }[] = [
  { id: "dark", label: "Dark" },
  { id: "light", label: "Light" },
  { id: "system", label: "System" },
];

/** Mobile Settings (03-03a): links to the existing views plus the theme setting. */
export function MobileSettings({
  onOpenImport,
  onOpenAudit,
}: {
  onOpenImport: () => void;
  onOpenAudit: () => void;
}) {
  const version = useAppInfoStore((s) => s.version);
  const theme = useSettingsStore((s) => s.theme);
  const setTheme = useSettingsStore((s) => s.setTheme);
  const isAndroid = useIsAndroid();

  return (
    <div className="m-screen" data-testid="m-settings">
      <div className="m-titlebar">
        <h1 className="m-titlebar__t">Settings</h1>
      </div>
      <div className="m-scroll">
        <div className="m-pad">
          <section className="card" aria-labelledby="m-settings-contexts">
            <h4 id="m-settings-contexts">Contexts</h4>
            <button type="button" className="m-link-row" onClick={onOpenImport} data-testid="m-settings-import">
              <Download size={16} strokeWidth={1.7} aria-hidden="true" />
              <span className="m-link-row__main">
                Import contexts
                {!isAndroid && (
                  <span className="m-link-row__sub">Only in the Android app; desktop reads your kubeconfig</span>
                )}
              </span>
              <ChevronRight size={16} strokeWidth={1.7} aria-hidden="true" />
            </button>
          </section>
          <section className="card" aria-labelledby="m-settings-appearance">
            <h4 id="m-settings-appearance">Appearance</h4>
            <div className="body">
              <div className="m-segmented" role="radiogroup" aria-label="Theme">
                {THEMES.map((t) => (
                  <button
                    key={t.id}
                    type="button"
                    role="radio"
                    aria-checked={theme === t.id}
                    className="btn m-segmented__btn"
                    onClick={() => {
                      setTheme(t.id);
                    }}
                  >
                    {t.label}
                  </button>
                ))}
              </div>
            </div>
          </section>
          <section className="card" aria-labelledby="m-settings-activity">
            <h4 id="m-settings-activity">Activity</h4>
            <button type="button" className="m-link-row" onClick={onOpenAudit} data-testid="m-settings-audit">
              <FileClock size={16} strokeWidth={1.7} aria-hidden="true" />
              <span className="m-link-row__main">Audit log</span>
              <ChevronRight size={16} strokeWidth={1.7} aria-hidden="true" />
            </button>
          </section>
          <section className="card" aria-labelledby="m-settings-about">
            <h4 id="m-settings-about">About</h4>
            <dl className="kv body m-settings__about">
              <dt>Version</dt>
              <dd data-testid="m-settings-version">{version === null ? "—" : `v${version}`}</dd>
            </dl>
          </section>
        </div>
      </div>
    </div>
  );
}
