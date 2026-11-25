import { useEffect, useRef } from "react";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebLinksAddon } from "@xterm/addon-web-links";
import "@xterm/xterm/css/xterm.css";
import { commands } from "../generated/bindings";
import { type TerminalStream } from "./terminalStream";
import { resolveTerminalFont, resolveTerminalTheme } from "./terminalTheme";

/** Trailing debounce for container resizes before refitting and signalling the remote PTY. */
export const RESIZE_DEBOUNCE_MS = 80;

export interface TerminalEmulatorProps {
  /** Output source; the terminal is mounted while it is set (also after the session ends). */
  stream: TerminalStream | null;
  /** Active session ID; resize signals are only sent while it is set. */
  sessionId: number | null;
  onStdin?: (data: string) => void;
  onResize?: (cols: number, rows: number) => void;
  testId?: string;
  ariaLabel?: string;
  placeholder?: React.ReactNode;
  autoFocus?: boolean;
}

/**
 * Opens terminal-supplied URLs only through the backend's validated, audited `open_in_browser`
 * command; never `window.open`.
 */
function openTerminalLink(uri: string): void {
  void commands
    .openInBrowser(uri)
    .then((res) => {
      if (res.status === "error") {
        console.warn("Failed to open terminal link:", res.error);
      }
    })
    .catch((err: unknown) => {
      console.warn("Failed to invoke openInBrowser:", err);
    });
}

export function TerminalEmulator({
  stream,
  sessionId,
  onStdin,
  onResize,
  testId = "exec-terminal",
  ariaLabel = "Terminal emulator",
  placeholder,
  autoFocus = true,
}: TerminalEmulatorProps) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const termRef = useRef<Terminal | null>(null);
  const onStdinRef = useRef(onStdin);
  const onResizeRef = useRef(onResize);
  const sessionIdRef = useRef(sessionId);
  /** Last size signalled, per session, so unchanged fits never reach the backend. */
  const lastSentRef = useRef<{ sessionId: number; cols: number; rows: number } | null>(null);

  useEffect(() => {
    onStdinRef.current = onStdin;
    onResizeRef.current = onResize;
  }, [onStdin, onResize]);

  const signalSize = (cols: number, rows: number) => {
    const id = sessionIdRef.current;
    if (typeof id !== "number" || cols <= 0 || rows <= 0) return;
    const last = lastSentRef.current;
    if (last !== null && last.sessionId === id && last.cols === cols && last.rows === rows) return;
    lastSentRef.current = { sessionId: id, cols, rows };
    onResizeRef.current?.(cols, rows);
  };
  const signalSizeRef = useRef(signalSize);
  useEffect(() => {
    signalSizeRef.current = signalSize;
  });

  useEffect(() => {
    if (stream === null) return;
    const container = containerRef.current;
    if (!container) return;

    const term = new Terminal({
      cursorBlink: true,
      cursorStyle: "block",
      ...resolveTerminalFont(),
      lineHeight: 1.2,
      scrollback: 5000,
      screenReaderMode: true,
      theme: resolveTerminalTheme(),
      // OSC 8 hyperlinks; xterm's default handler would call window.open.
      linkHandler: {
        activate: (_event, uri) => {
          openTerminalLink(uri);
        },
      },
    });
    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    term.loadAddon(
      new WebLinksAddon((_event, uri) => {
        openTerminalLink(uri);
      }),
    );
    term.open(container);
    termRef.current = term;

    const fit = () => {
      if (container.clientWidth === 0 || container.clientHeight === 0) return;
      try {
        fitAddon.fit();
      } catch {
        // No measurable renderer yet (e.g. hidden container); the next resize retries.
        return;
      }
      signalSizeRef.current(term.cols, term.rows);
    };

    let fitTimer: ReturnType<typeof setTimeout> | null = null;
    const scheduleFit = () => {
      if (fitTimer !== null) clearTimeout(fitTimer);
      fitTimer = setTimeout(() => {
        fitTimer = null;
        fit();
      }, RESIZE_DEBOUNCE_MS);
    };

    fit();

    const dataSub = term.onData((data) => {
      onStdinRef.current?.(data);
    });
    const unsubscribe = stream.subscribe((chunk) => {
      term.write(chunk);
    });

    const resizeObserver =
      typeof ResizeObserver === "undefined" ? null : new ResizeObserver(scheduleFit);
    resizeObserver?.observe(container);
    window.addEventListener("resize", scheduleFit);

    const themeObserver =
      typeof MutationObserver === "undefined"
        ? null
        : new MutationObserver(() => {
            term.options.theme = resolveTerminalTheme();
          });
    themeObserver?.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ["data-theme", "class"],
    });

    return () => {
      if (fitTimer !== null) clearTimeout(fitTimer);
      window.removeEventListener("resize", scheduleFit);
      resizeObserver?.disconnect();
      themeObserver?.disconnect();
      unsubscribe();
      dataSub.dispose();
      term.dispose();
      termRef.current = null;
    };
  }, [stream]);

  // A new session gets the terminal's current size as soon as it has an ID.
  useEffect(() => {
    sessionIdRef.current = sessionId;
    const term = termRef.current;
    if (typeof sessionId !== "number" || !term) return;
    signalSizeRef.current(term.cols, term.rows);
    if (autoFocus) term.focus();
  }, [sessionId, autoFocus]);

  return (
    <div
      data-testid={testId}
      className="exec-panel__terminal"
      role="region"
      aria-label={ariaLabel}
      onClick={(e) => {
        // Clicks in the placeholder (e.g. the protected-context confirm input) keep their focus.
        if (e.target === e.currentTarget || containerRef.current?.contains(e.target as Node)) {
          termRef.current?.focus();
        }
      }}
    >
      {placeholder}
      <div
        ref={containerRef}
        className="xterm-container"
        hidden={stream === null}
      />
    </div>
  );
}
