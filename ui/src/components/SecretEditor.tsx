import { useEffect, useState } from "react";
import { commands, type ErrorKind, type IpcError } from "../generated/bindings";

export type SecretCodec = (data: Record<string, string>) => Promise<Record<string, string>>;

export interface SecretEditorProps {
  /** Key-value data map as stored in the Secret's `data` (base64-encoded values). */
  data?: Record<string, string>;
  /** Optional secret Kubernetes resource representation. */
  secret?: {
    metadata?: { name?: string; namespace?: string };
    data?: Record<string, string>;
    stringData?: Record<string, string>;
  };
  name?: string;
  namespace?: string;
  readOnly?: boolean;
  /** Whether decoded plaintext is visible initially. Defaults to false (masked). */
  initialRevealed?: boolean;
  /** Decodes base64 values to plaintext. Defaults to the Rust `decode_secret_values` command. */
  decode?: SecretCodec;
  /** Encodes plaintext values to base64. Defaults to the Rust `encode_secret_values` command. */
  encode?: SecretCodec;
  /** Callback invoked on save with decoded and base64-encoded values. */
  onSave?: (decodedData: Record<string, string>, base64Data: Record<string, string>) => void;
  onCancel?: () => void;
}

interface SecretEntry {
  id: string;
  key: string;
  value: string;
}

type CodecErrorKind = "decode" | "encode";

interface CodecError {
  op: CodecErrorKind;
  message: string;
}

/** Message from a rejected `IpcError`, an `Error`, or anything else thrown. */
function errorMessage(err: unknown): string {
  if (err instanceof Error) return err.message;
  if (typeof err === "object" && err !== null && "message" in err) {
    return String(err.message);
  }
  return String(err);
}

/** A typed `IpcError` from the secret codec commands, rethrown as an `Error`. */
class SecretCodecError extends Error {
  readonly kind: ErrorKind;
  constructor(error: IpcError) {
    super(error.message);
    this.name = "SecretCodecError";
    this.kind = error.kind;
  }
}

const defaultDecode: SecretCodec = async (data) => {
  const res = await commands.decodeSecretValues(data);
  if (res.status === "error") throw new SecretCodecError(res.error);
  return res.data;
};

const defaultEncode: SecretCodec = async (data) => {
  const res = await commands.encodeSecretValues(data);
  if (res.status === "error") throw new SecretCodecError(res.error);
  return res.data;
};

function toEntries(values: Record<string, string>): SecretEntry[] {
  return Object.entries(values).map(([key, value], idx) => ({
    id: `${key}-${String(idx)}`,
    key,
    value,
  }));
}

export function SecretEditor({
  data,
  secret,
  name,
  namespace,
  readOnly = false,
  initialRevealed = false,
  decode = defaultDecode,
  encode = defaultEncode,
  onSave,
  onCancel,
}: SecretEditorProps) {
  const [showPlaintext, setShowPlaintext] = useState<boolean>(initialRevealed);
  const [entries, setEntries] = useState<SecretEntry[]>([]);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<CodecError | null>(null);

  // Decode exactly once on mount (also under StrictMode's double effect run); later prop
  // changes do not overwrite user edits.
  const [decodedPromise] = useState(() => {
    const encoded = data ?? secret?.data;
    // `stringData` is already plaintext and needs no decoding.
    return encoded ? decode(encoded) : Promise.resolve({ ...(secret?.stringData ?? {}) });
  });
  const [encodedKeys] = useState(() => Object.keys(data ?? secret?.data ?? {}));

  useEffect(() => {
    let cancelled = false;
    decodedPromise.then(
      (decoded) => {
        if (cancelled) return;
        setEntries(toEntries(decoded));
        setLoading(false);
      },
      (err: unknown) => {
        if (cancelled) return;
        setError({ op: "decode", message: errorMessage(err) });
        // Keep the keys visible but never expose the undecodable raw values.
        setEntries(toEntries(Object.fromEntries(encodedKeys.map((k) => [k, ""]))));
        setLoading(false);
      },
    );
    return () => {
      cancelled = true;
    };
  }, [decodedPromise, encodedKeys]);

  const decodeFailed = error?.op === "decode";
  const revealed = showPlaintext && !decodeFailed;
  const editable = !readOnly && !loading && !decodeFailed;

  const effectiveName = name ?? secret?.metadata?.name;
  const effectiveNamespace = namespace ?? secret?.metadata?.namespace;

  const handleToggleReveal = () => {
    setShowPlaintext((prev) => !prev);
  };

  const handleUpdateKey = (id: string, newKey: string) => {
    setEntries((prev) =>
      prev.map((e) => (e.id === id ? { ...e, key: newKey } : e)),
    );
  };

  const handleUpdateValue = (id: string, newValue: string) => {
    setEntries((prev) =>
      prev.map((e) => (e.id === id ? { ...e, value: newValue } : e)),
    );
  };

  const handleAddEntry = () => {
    setEntries((prev) => [
      ...prev,
      {
        id: `entry-${String(Date.now())}-${Math.random().toString(36).substring(2, 7)}`,
        key: "",
        value: "",
      },
    ]);
  };

  const handleRemoveEntry = (id: string) => {
    setEntries((prev) => prev.filter((e) => e.id !== id));
  };

  const handleSave = async () => {
    const decodedResult: Record<string, string> = {};
    for (const entry of entries) {
      if (!entry.key.trim()) continue;
      decodedResult[entry.key] = entry.value;
    }
    setSaving(true);
    try {
      const base64Result = await encode(decodedResult);
      setError(null);
      onSave?.(decodedResult, base64Result);
    } catch (err: unknown) {
      setError({ op: "encode", message: errorMessage(err) });
    } finally {
      setSaving(false);
    }
  };

  return (
    <div className="secret-editor" aria-label="Secret Editor">
      <div className="secret-editor__toolbar">
        <div className="secret-editor__title-group">
          <h3 className="secret-editor__title">
            {effectiveName ? `Secret: ${effectiveName}` : "Secret Editor"}
          </h3>
          {effectiveNamespace && (
            <span className="badge secret-editor__ns-badge">{effectiveNamespace}</span>
          )}
        </div>

        <div className="secret-editor__actions">
          <button
            type="button"
            className="btn secret-editor__reveal-btn"
            aria-label="Reveal / Decode"
            title="Reveal / Decode"
            aria-pressed={revealed}
            disabled={loading || decodeFailed}
            onClick={handleToggleReveal}
          >
            Reveal / Decode
          </button>

          <label className="secret-editor__plaintext-toggle">
            <input
              type="checkbox"
              role="switch"
              aria-label="Plaintext toggle"
              checked={revealed}
              disabled={loading || decodeFailed}
              onChange={(e) => {
                setShowPlaintext(e.target.checked);
              }}
            />
            <span>Plaintext</span>
          </label>

          {!readOnly && (
            <button
              type="button"
              className="btn btn--primary secret-editor__save-btn"
              disabled={!editable || saving}
              onClick={() => {
                void handleSave();
              }}
            >
              {saving ? "Saving…" : "Save"}
            </button>
          )}

          {onCancel && (
            <button
              type="button"
              className="btn secret-editor__cancel-btn"
              onClick={() => {
                onCancel();
              }}
            >
              Cancel
            </button>
          )}
        </div>
      </div>

      {readOnly && (
        <div
          role="alert"
          data-testid="secret-readonly-notice"
          className="secret-editor__readonly-notice"
        >
          Secret is read-only
        </div>
      )}

      {error && (
        <div role="alert" data-testid="secret-codec-error" className="secret-editor__readonly-notice">
          {error.op === "decode" ? "Cannot decode secret: " : "Cannot encode secret: "}
          {error.message}
        </div>
      )}

      <div className="secret-editor__entries" role="table" aria-label="Secret key-value pairs">
        <div className="secret-editor__row secret-editor__header-row" role="row">
          <span className="secret-editor__header-cell" role="columnheader">
            Key
          </span>
          <span className="secret-editor__header-cell" role="columnheader">
            Value
          </span>
          {!readOnly && (
            <span className="secret-editor__header-cell secret-editor__header-cell--actions" role="columnheader">
              Actions
            </span>
          )}
        </div>

        {loading ? (
          <p className="empty-state" role="status" aria-busy="true">
            Decoding secret…
          </p>
        ) : entries.length === 0 ? (
          <p className="empty-state">No keys in secret</p>
        ) : (
          entries.map((entry) => (
            <div key={entry.id} className="secret-editor__row" role="row">
              <div className="secret-editor__cell" role="cell">
                <input
                  type="text"
                  className="secret-editor__key-input"
                  aria-label={`Key for ${entry.key || "entry"}`}
                  placeholder="KEY"
                  value={entry.key}
                  readOnly={!editable}
                  onChange={(e) => {
                    handleUpdateKey(entry.id, e.target.value);
                  }}
                />
              </div>

              <div className="secret-editor__cell" role="cell">
                <div className="secret-editor__value-container">
                  <span
                    className="secret-editor__value-display"
                    data-testid={`secret-value-${entry.key}`}
                  >
                    {revealed ? entry.value : "••••••••"}
                  </span>
                  <input
                    type={revealed ? "text" : "password"}
                    className="secret-editor__value-input"
                    aria-label={`Value for ${entry.key || "entry"}`}
                    value={entry.value}
                    readOnly={!editable}
                    onChange={(e) => {
                      handleUpdateValue(entry.id, e.target.value);
                    }}
                  />
                </div>
              </div>

              {!readOnly && (
                <div className="secret-editor__cell secret-editor__cell--actions" role="cell">
                  <button
                    type="button"
                    className="btn btn--danger secret-editor__remove-btn"
                    aria-label={`Delete ${entry.key || "entry"}`}
                    disabled={!editable}
                    onClick={() => {
                      handleRemoveEntry(entry.id);
                    }}
                  >
                    Delete
                  </button>
                </div>
              )}
            </div>
          ))
        )}
      </div>

      {!readOnly && (
        <div className="secret-editor__footer">
          <button
            type="button"
            className="btn secret-editor__add-btn"
            disabled={!editable}
            onClick={() => {
              handleAddEntry();
            }}
          >
            + Add Key
          </button>
        </div>
      )}
    </div>
  );
}
