import { useMemo, useState, type SyntheticEvent } from "react";
import { usePortForwardsStore } from "../stores/portForwards";
import { containerPortsFromYaml } from "./containerPorts";

function parsePort(value: string, allowZero: boolean): number | null {
  if (!/^\d+$/.test(value.trim())) return null;
  const port = Number(value);
  if (port > 65535 || (port === 0 && !allowZero)) return null;
  return port;
}

export interface PortForwardFormProps {
  context: string;
  namespace: string;
  pod: string;
  yaml: string;
  initialRemotePort?: number | string | undefined;
}

/** Starts a 127.0.0.1 port-forward to the pod; active forwards are listed in the status bar. */
export function PortForwardForm({
  context,
  namespace,
  pod,
  yaml,
  initialRemotePort,
}: PortForwardFormProps) {
  const startForward = usePortForwardsStore((s) => s.startForward);
  const saveBookmark = usePortForwardsStore((s) => s.saveBookmark);
  const declared = useMemo(() => containerPortsFromYaml(yaml), [yaml]);
  const [remote, setRemote] = useState(
    initialRemotePort !== undefined ? String(initialRemotePort) : ""
  );
  const [local, setLocal] = useState("");
  const [saveAsBookmark, setSaveAsBookmark] = useState(false);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ role: "status" | "alert"; text: string } | null>(null);

  const remoteValue =
    remote === "" && initialRemotePort === undefined && declared[0] !== undefined
      ? String(declared[0])
      : remote;

  const submit = async (e: SyntheticEvent) => {
    e.preventDefault();
    const remotePort = parsePort(remoteValue, false);
    const localPort = local.trim() === "" ? 0 : parsePort(local, true);
    if (remotePort === null || localPort === null) {
      setMessage({ role: "alert", text: "Ports must be numbers between 1 and 65535." });
      return;
    }
    setBusy(true);
    setMessage(null);
    const item = await startForward(context, namespace, pod, remotePort, localPort);
    setBusy(false);
    if (item === null) {
      const error = usePortForwardsStore.getState().error ?? "unknown error";
      setMessage({ role: "alert", text: `Port-forward failed: ${error}` });
    } else {
      if (saveAsBookmark) {
        void saveBookmark({
          id: `${context}/${namespace}/pod/${pod}/${String(remotePort)}`,
          context,
          namespace,
          targetKind: "pod",
          targetName: pod,
          containerPort: remotePort,
          localPort: item.localPort,
          label: `${pod}:${String(remotePort)}`,
        });
      }
      setMessage({ role: "status", text: `Forwarding ${item.browserUrl} → ${pod}:${String(remotePort)}` });
    }
  };

  return (
    <form
      className="port-forward-form"
      aria-label="Start port-forward"
      onSubmit={(e) => {
        void submit(e);
      }}
    >
      <label className="port-forward-form__field">
        <span>Pod port</span>
        <input
          className="port-forward-form__input"
          inputMode="numeric"
          list={declared.length > 0 ? `pf-ports-${pod}` : undefined}
          value={remoteValue}
          placeholder="8080"
          onChange={(e) => {
            setRemote(e.target.value);
          }}
        />
        {declared.length > 0 && (
          <datalist id={`pf-ports-${pod}`}>
            {declared.map((p) => (
              <option key={p} value={p} />
            ))}
          </datalist>
        )}
      </label>
      <label className="port-forward-form__field">
        <span>Local port</span>
        <input
          className="port-forward-form__input"
          inputMode="numeric"
          value={local}
          placeholder="auto"
          onChange={(e) => {
            setLocal(e.target.value);
          }}
        />
      </label>
      <label className="port-forward-form__checkbox">
        <input
          type="checkbox"
          checked={saveAsBookmark}
          onChange={(e) => {
            setSaveAsBookmark(e.target.checked);
          }}
        />
        <span>Save as bookmark</span>
      </label>
      <button type="submit" className="resource-drawer__btn" disabled={busy || remoteValue === ""}>
        {busy ? "Starting…" : "Forward"}
      </button>
      {message !== null && (
        <p role={message.role} className={`port-forward-form__msg port-forward-form__msg--${message.role}`}>
          {message.text}
        </p>
      )}
    </form>
  );
}
