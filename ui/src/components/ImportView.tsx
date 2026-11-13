import { type SyntheticEvent, useEffect, useId, useRef, useState } from "react";
import { describeImportProblem, useImporterStore } from "../stores/importer";
import { authLabel } from "./authLabel";

/**
 * Android "Import contexts" screen (03-02c): a kubeconfig file or pasted text, a bearer token for
 * one API server, and the list of imported contexts. Plain on purpose; mobile styling is 03-03.
 */
export function ImportView() {
  const message = useImporterStore((s) => s.message);
  const error = useImporterStore((s) => s.error);
  const loadImported = useImporterStore((s) => s.loadImported);

  useEffect(() => {
    void loadImported();
  }, [loadImported]);

  return (
    <div className="import-view" aria-labelledby="import-view-title">
      <h2 id="import-view-title" className="import-view__title">
        Import contexts
      </h2>
      <p className="import-view__hint">
        Imported credentials are stored encrypted on this device and are never backed up.
      </p>
      {error && (
        <p className="error-state import-view__msg" role="alert">
          {error.message}
        </p>
      )}
      {message && (
        <p className="import-view__msg import-view__msg--ok" role="status">
          {message}
        </p>
      )}
      <FileSection />
      <TokenSection />
      <ImportedSection />
    </div>
  );
}

function FileSection() {
  const busy = useImporterStore((s) => s.busy);
  const pasted = useImporterStore((s) => s.pasted);
  const preview = useImporterStore((s) => s.preview);
  const pickFile = useImporterStore((s) => s.pickFile);
  const setPasted = useImporterStore((s) => s.setPasted);
  const previewPasted = useImporterStore((s) => s.previewPasted);
  const pasteId = useId();

  return (
    <section className="import-view__section" aria-labelledby={`${pasteId}-h`}>
      <h3 id={`${pasteId}-h`} className="import-view__heading">
        From a file
      </h3>
      <button
        type="button"
        className="btn import-view__btn"
        disabled={busy}
        onClick={() => {
          void pickFile();
        }}
      >
        Choose kubeconfig file
      </button>
      <label className="import-view__field" htmlFor={pasteId}>
        <span>Paste kubeconfig</span>
        <textarea
          id={pasteId}
          className="import-view__input import-view__textarea"
          rows={6}
          value={pasted}
          spellCheck={false}
          autoCapitalize="off"
          autoCorrect="off"
          autoComplete="off"
          onChange={(e) => {
            setPasted(e.target.value);
          }}
        />
      </label>
      <button
        type="button"
        className="btn import-view__btn"
        disabled={busy || pasted.trim() === ""}
        onClick={() => {
          void previewPasted();
        }}
      >
        Preview
      </button>
      {preview && <PreviewList />}
    </section>
  );
}

function PreviewList() {
  const busy = useImporterStore((s) => s.busy);
  const preview = useImporterStore((s) => s.preview);
  const selected = useImporterStore((s) => s.selected);
  const replaceExisting = useImporterStore((s) => s.replaceExisting);
  const toggleSelected = useImporterStore((s) => s.toggleSelected);
  const setReplaceExisting = useImporterStore((s) => s.setReplaceExisting);
  const commit = useImporterStore((s) => s.commit);
  const cancel = useImporterStore((s) => s.cancel);
  const baseId = useId();
  if (!preview) return null;

  const selectedExisting = preview.contexts.some((c) => c.exists && selected.includes(c.name));
  const n = selected.length;

  return (
    <div className="import-view__preview" aria-label="Contexts in the kubeconfig" role="group">
      {preview.warnings.length > 0 && (
        <ul className="notice import-view__warnings" aria-label="Import warnings">
          {preview.warnings.map((w) => (
            <li key={w}>{w}</li>
          ))}
        </ul>
      )}
      {preview.contexts.length === 0 ? (
        <p className="empty-state">The kubeconfig has no contexts.</p>
      ) : (
        <ul className="import-view__list">
          {preview.contexts.map((c, i) => {
            const id = `${baseId}-c${String(i)}`;
            const problemId = `${id}-problem`;
            return (
              <li key={c.name} className="import-view__candidate" data-testid={`candidate-${c.name}`}>
                <input
                  id={id}
                  type="checkbox"
                  className="import-view__check"
                  checked={selected.includes(c.name)}
                  disabled={c.problem !== null}
                  aria-describedby={c.problem ? problemId : undefined}
                  onChange={() => {
                    toggleSelected(c.name);
                  }}
                />
                <label htmlFor={id} className="import-view__candidate-main">
                  <span className="import-view__name">{c.name}</span>
                  <span className="import-view__server">{c.server ?? "no server"}</span>
                  <span className="import-view__badges">
                    <span className="badge">{authLabel(c.auth)}</span>
                    {c.exists && <span className="badge badge--warning">already imported</span>}
                  </span>
                </label>
                {c.problem && (
                  <p id={problemId} className="import-view__problem">
                    Context “{c.name}” {describeImportProblem(c.problem)}.
                  </p>
                )}
              </li>
            );
          })}
        </ul>
      )}
      {selectedExisting && (
        <label className="import-view__checkbox">
          <input
            type="checkbox"
            checked={replaceExisting}
            onChange={(e) => {
              setReplaceExisting(e.target.checked);
            }}
          />
          <span>Replace existing</span>
        </label>
      )}
      <div className="import-view__actions">
        <button
          type="button"
          className="btn import-view__btn"
          disabled={busy || n === 0}
          onClick={() => {
            void commit();
          }}
        >
          Import {n} context{n === 1 ? "" : "s"}
        </button>
        <button
          type="button"
          className="btn import-view__btn"
          disabled={busy}
          onClick={() => {
            void cancel();
          }}
        >
          Cancel
        </button>
      </div>
    </div>
  );
}

function TokenSection() {
  const busy = useImporterStore((s) => s.busy);
  const token = useImporterStore((s) => s.token);
  const setToken = useImporterStore((s) => s.setToken);
  const submitToken = useImporterStore((s) => s.submitToken);
  const id = useId();
  const complete =
    token.name.trim() !== "" && token.server.trim() !== "" && token.token !== "";

  const onSubmit = (e: SyntheticEvent) => {
    e.preventDefault();
    if (!complete || busy) return;
    void submitToken();
  };

  return (
    <section className="import-view__section" aria-labelledby={`${id}-h`}>
      <h3 id={`${id}-h`} className="import-view__heading">
        Paste a token
      </h3>
      <form className="import-view__form" onSubmit={onSubmit} autoComplete="off">
        <label className="import-view__field" htmlFor={`${id}-name`}>
          <span>Context name</span>
          <input
            id={`${id}-name`}
            className="import-view__input"
            value={token.name}
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            required
            onChange={(e) => {
              setToken({ name: e.target.value });
            }}
          />
        </label>
        <label className="import-view__field" htmlFor={`${id}-server`}>
          <span>API server URL</span>
          <input
            id={`${id}-server`}
            className="import-view__input"
            type="url"
            inputMode="url"
            placeholder="https://"
            value={token.server}
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            required
            onChange={(e) => {
              setToken({ server: e.target.value });
            }}
          />
        </label>
        <label className="import-view__field" htmlFor={`${id}-token`}>
          <span>Token</span>
          <input
            id={`${id}-token`}
            className="import-view__input"
            type="password"
            autoComplete="off"
            autoCapitalize="off"
            autoCorrect="off"
            spellCheck={false}
            value={token.token}
            required
            onChange={(e) => {
              setToken({ token: e.target.value });
            }}
          />
        </label>
        <label className="import-view__field" htmlFor={`${id}-ca`}>
          <span>CA certificate (PEM, optional)</span>
          <textarea
            id={`${id}-ca`}
            className="import-view__input import-view__textarea"
            rows={4}
            placeholder="-----BEGIN CERTIFICATE-----"
            value={token.caPem}
            autoCapitalize="off"
            autoCorrect="off"
            autoComplete="off"
            spellCheck={false}
            onChange={(e) => {
              setToken({ caPem: e.target.value });
            }}
          />
        </label>
        <label className="import-view__checkbox">
          <input
            type="checkbox"
            checked={token.insecureSkipTlsVerify}
            aria-describedby={`${id}-insecure`}
            onChange={(e) => {
              setToken({ insecureSkipTlsVerify: e.target.checked });
            }}
          />
          <span>Skip TLS verification (test clusters only)</span>
        </label>
        <p
          id={`${id}-insecure`}
          className="import-view__warn"
          data-active={token.insecureSkipTlsVerify ? "true" : undefined}
        >
          Without verification anyone on the network path can impersonate the server and read
          the token.
        </p>
        <div className="import-view__actions">
          <button type="submit" className="btn import-view__btn" disabled={busy || !complete}>
            Import token context
          </button>
        </div>
      </form>
    </section>
  );
}

function ImportedSection() {
  const busy = useImporterStore((s) => s.busy);
  const imported = useImporterStore((s) => s.imported);
  const status = useImporterStore((s) => s.importedStatus);
  const remove = useImporterStore((s) => s.remove);
  const importedError = useImporterStore((s) => s.importedError);
  const storeBroken = useImporterStore((s) => s.storeBroken);
  const [confirming, setConfirming] = useState<string | null>(null);
  const noRef = useRef<HTMLButtonElement | null>(null);
  const id = useId();

  useEffect(() => {
    // The safe choice gets focus when the inline confirmation appears.
    if (confirming !== null) noRef.current?.focus();
  }, [confirming]);

  return (
    <section className="import-view__section" aria-labelledby={`${id}-h`}>
      <h3 id={`${id}-h`} className="import-view__heading">
        Imported contexts
      </h3>
      {importedError && (
        <p className="error-state import-view__msg" role="alert">
          {importedError.message}
        </p>
      )}
      {storeBroken && <ResetStore />}
      {imported === null ? (
        status === "error" ? null : (
          <p className="empty-state" role="status">
            Loading…
          </p>
        )
      ) : imported.length === 0 ? (
        <p className="empty-state">No imported contexts yet.</p>
      ) : (
        <ul className="import-view__list" aria-label="Imported contexts">
          {imported.map((name) => (
            <li key={name} className="import-view__imported">
              <span className="import-view__name">{name}</span>
              {confirming === name ? (
                <span className="import-view__confirm" role="group" aria-label={`Remove ${name}?`}>
                  <span aria-hidden="true">Remove?</span>
                  <button
                    type="button"
                    className="btn danger import-view__btn"
                    disabled={busy}
                    aria-label={`Yes, remove ${name}`}
                    onClick={() => {
                      setConfirming(null);
                      void remove(name);
                    }}
                  >
                    Yes
                  </button>
                  <button
                    ref={noRef}
                    type="button"
                    className="btn import-view__btn"
                    aria-label={`No, keep ${name}`}
                    onClick={() => {
                      setConfirming(null);
                    }}
                  >
                    No
                  </button>
                </span>
              ) : (
                <button
                  type="button"
                  className="btn import-view__btn"
                  disabled={busy}
                  aria-label={`Remove ${name}`}
                  onClick={() => {
                    setConfirming(name);
                  }}
                >
                  Remove
                </button>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** Deletes the stored contexts when they can no longer be decrypted; inline confirmation. */
function ResetStore() {
  const busy = useImporterStore((s) => s.busy);
  const resetStore = useImporterStore((s) => s.resetStore);
  const [confirming, setConfirming] = useState(false);
  const noRef = useRef<HTMLButtonElement | null>(null);

  useEffect(() => {
    if (confirming) noRef.current?.focus();
  }, [confirming]);

  return confirming ? (
    <span className="import-view__confirm" role="group" aria-label="Delete stored contexts?">
      <span aria-hidden="true">Delete?</span>
      <button
        type="button"
        className="btn danger import-view__btn"
        disabled={busy}
        aria-label="Yes, delete stored contexts"
        onClick={() => {
          setConfirming(false);
          void resetStore();
        }}
      >
        Yes
      </button>
      <button
        ref={noRef}
        type="button"
        className="btn import-view__btn"
        aria-label="No, keep stored contexts"
        onClick={() => {
          setConfirming(false);
        }}
      >
        No
      </button>
    </span>
  ) : (
    <button
      type="button"
      className="btn danger import-view__btn"
      disabled={busy}
      onClick={() => {
        setConfirming(true);
      }}
    >
      Delete stored contexts
    </button>
  );
}
