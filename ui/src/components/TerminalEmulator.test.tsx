import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { FitAddon } from "@xterm/addon-fit";
import * as webLinksModule from "@xterm/addon-web-links";
import { Terminal } from "@xterm/xterm";
import { RESIZE_DEBOUNCE_MS, TerminalEmulator } from "./TerminalEmulator";
import { TERMINAL_TAIL_LIMIT, TerminalStream } from "./terminalStream";
import { resolveTerminalFont, resolveTerminalTheme } from "./terminalTheme";
import { commands } from "../generated/bindings";

const root = document.documentElement;

function setTokens(tokens: Record<string, string>) {
  for (const [name, value] of Object.entries(tokens)) root.style.setProperty(name, value);
}

/** Text xterm currently renders (the visible viewport only, so it is bounded). */
function renderedText(testId = "exec-terminal"): string {
  return screen.getByTestId(testId).querySelector(".xterm-rows")?.textContent ?? "";
}

/** Makes FitAddon.fit() resize the terminal to the given size, as a real layout would. */
function mockFit(size: { cols: number; rows: number }) {
  return vi.spyOn(FitAddon.prototype, "fit").mockImplementation(function (this: FitAddon) {
    const internal = this as unknown as { _terminal?: Terminal };
    internal._terminal?.resize(size.cols, size.rows);
  });
}

/** Records every Terminal that the component opens. */
function captureTerminals(): Terminal[] {
  const instances: Terminal[] = [];
  const proto = Terminal.prototype as unknown as {
    open: (this: Terminal, el: HTMLElement) => void;
  };
  const actualOpen = proto.open;
  vi.spyOn(proto, "open").mockImplementation(function (this: Terminal, el: HTMLElement) {
    instances.push(this);
    actualOpen.call(this, el);
  });
  return instances;
}

afterEach(() => {
  vi.restoreAllMocks();
  vi.useRealTimers();
  root.removeAttribute("style");
  root.removeAttribute("data-theme");
  root.classList.remove("light");
});

describe("05-02 TerminalStream", () => {
  it("forwards chunks to the subscriber and replays only a bounded tail", () => {
    const stream = new TerminalStream();
    stream.write("early ");
    const seen: string[] = [];
    const unsubscribe = stream.subscribe((c) => seen.push(c));
    stream.write("late");
    expect(seen).toEqual(["early ", "late"]);

    unsubscribe();
    stream.write("ignored");
    expect(seen).toEqual(["early ", "late"]);
  });

  it("never retains more than the tail limit, whatever the session length", () => {
    const stream = new TerminalStream();
    const chunk = "x".repeat(4096);
    for (let i = 0; i < 2000; i++) {
      stream.write(chunk);
      expect(stream.retainedLength()).toBeLessThanOrEqual(2 * TERMINAL_TAIL_LIMIT);
    }
    stream.write("y".repeat(5 * TERMINAL_TAIL_LIMIT));
    expect(stream.retainedLength()).toBeLessThanOrEqual(2 * TERMINAL_TAIL_LIMIT);
    stream.write("END");
    const tail = stream.tail();
    expect(tail.length).toBe(TERMINAL_TAIL_LIMIT);
    expect(tail.endsWith("END")).toBe(true);
  });
});

describe("05-02 terminal theme", () => {
  it("derives the xterm theme from the active palette tokens", () => {
    setTokens({ "--bg-terminal": "#0b0d10", "--fg-terminal": "#d7dae0", "--accent": "#4c8dff" });
    const dark = resolveTerminalTheme();
    expect(dark.background).toBe("#0b0d10");
    expect(dark.foreground).toBe("#d7dae0");
    expect(dark.cursor).toBe("#4c8dff");
    expect(dark.selectionBackground).toBe("rgba(76, 141, 255, 0.35)");
    expect(dark.red).toBe("#f85149");

    setTokens({ "--bg-terminal": "#ffffff", "--fg-terminal": "#1f2328", "--accent": "#2f6fec" });
    const light = resolveTerminalTheme();
    expect(light.background).toBe("#ffffff");
    expect(light.foreground).toBe("#1f2328");
    expect(light.cursor).toBe("#2f6fec");
    // ANSI colours switch to the set that is readable on a light background.
    expect(light.red).toBe("#cf222e");
  });

  it("takes the font from --font-mono and --fs-s", () => {
    setTokens({ "--font-mono": '"JetBrains Mono", monospace', "--fs-s": "13px" });
    expect(resolveTerminalFont()).toEqual({
      fontFamily: '"JetBrains Mono", monospace',
      fontSize: 13,
    });
  });
});

describe("05-02 TerminalEmulator (xterm.js)", () => {
  it("renders the placeholder and no terminal before a stream exists", () => {
    render(
      <TerminalEmulator
        stream={null}
        sessionId={null}
        placeholder={<div data-testid="test-placeholder">Select shell</div>}
      />,
    );
    expect(screen.getByTestId("test-placeholder")).toHaveTextContent("Select shell");
    expect(screen.getByTestId("exec-terminal").querySelector(".xterm")).toBeNull();
  });

  it("streams chunks straight into xterm and forwards keystrokes via onStdin", async () => {
    const user = userEvent.setup();
    const onStdin = vi.fn();
    const stream = new TerminalStream();
    stream.write("connecting...\r\n");

    render(<TerminalEmulator stream={stream} sessionId={10} onStdin={onStdin} />);

    await waitFor(() => {
      expect(renderedText()).toContain("connecting...");
    });
    act(() => {
      stream.write("root@pod:/# ");
    });
    await waitFor(() => {
      expect(renderedText()).toContain("root@pod:/#");
    });
    // No full-output mirror in the DOM: only xterm's bounded viewport.
    expect(screen.queryByTestId("exec-output")).toBeNull();

    const textarea = screen
      .getByTestId("exec-terminal")
      .querySelector<HTMLTextAreaElement>(".xterm-helper-textarea");
    if (!textarea) throw new Error("Missing textarea");

    await user.type(textarea, "top");
    expect(onStdin).toHaveBeenCalledWith("t");
    expect(onStdin).toHaveBeenCalledWith("o");
    expect(onStdin).toHaveBeenCalledWith("p");

    fireEvent.keyDown(textarea, { key: "Enter", keyCode: 13 });
    expect(onStdin).toHaveBeenCalledWith("\r");
    fireEvent.keyDown(textarea, { key: "Backspace", keyCode: 8 });
    expect(onStdin).toHaveBeenCalledWith("\x7f");
    fireEvent.keyDown(textarea, { key: "c", ctrlKey: true, keyCode: 67 });
    expect(onStdin).toHaveBeenCalledWith("\x03");
  });

  it("enables xterm's screen reader mode instead of a hidden output mirror", () => {
    const stream = new TerminalStream();
    render(<TerminalEmulator stream={stream} sessionId={1} />);
    expect(
      screen.getByTestId("exec-terminal").querySelector(".xterm-accessibility"),
    ).toBeInTheDocument();
  });

  it("debounces container resizes and signals only changed sizes for the active session", () => {
    vi.useFakeTimers();
    const size = { cols: 120, rows: 36 };
    mockFit(size);
    vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(800);
    vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(400);
    const onResize = vi.fn();
    const stream = new TerminalStream();

    const { rerender } = render(
      <TerminalEmulator stream={stream} sessionId={null} onResize={onResize} />,
    );
    // No session yet: nothing to resize.
    expect(onResize).not.toHaveBeenCalled();

    rerender(<TerminalEmulator stream={stream} sessionId={15} onResize={onResize} />);
    expect(onResize).toHaveBeenCalledTimes(1);
    expect(onResize).toHaveBeenLastCalledWith(120, 36);

    // A burst of resize events collapses into one fit after the debounce.
    size.cols = 140;
    size.rows = 42;
    for (let i = 0; i < 50; i++) window.dispatchEvent(new Event("resize"));
    expect(onResize).toHaveBeenCalledTimes(1);
    act(() => {
      vi.advanceTimersByTime(RESIZE_DEBOUNCE_MS);
    });
    expect(onResize).toHaveBeenCalledTimes(2);
    expect(onResize).toHaveBeenLastCalledWith(140, 42);

    // Unchanged size is not re-sent.
    window.dispatchEvent(new Event("resize"));
    act(() => {
      vi.advanceTimersByTime(RESIZE_DEBOUNCE_MS);
    });
    expect(onResize).toHaveBeenCalledTimes(2);

    // A reconnect on the same terminal gets the current size even though it did not change.
    rerender(<TerminalEmulator stream={stream} sessionId={null} onResize={onResize} />);
    rerender(<TerminalEmulator stream={stream} sessionId={16} onResize={onResize} />);
    expect(onResize).toHaveBeenCalledTimes(3);
    expect(onResize).toHaveBeenLastCalledWith(140, 42);
  });

  it("updates the xterm theme live when the app theme changes", async () => {
    setTokens({ "--bg-terminal": "#0b0d10", "--accent": "#4c8dff" });
    const instances = captureTerminals();
    render(<TerminalEmulator stream={new TerminalStream()} sessionId={1} />);
    const term = instances[0];
    if (!term) throw new Error("Terminal was not opened");
    expect(term.options.theme?.background).toBe("#0b0d10");

    setTokens({ "--bg-terminal": "#ffffff", "--accent": "#2f6fec" });
    root.setAttribute("data-theme", "light");

    await waitFor(() => {
      expect(term.options.theme?.background).toBe("#ffffff");
    });
    expect(term.options.theme?.cursor).toBe("#2f6fec");
  });

  it("routes link activation through openInBrowser and never window.open", () => {
    let capturedHandler: ((event: MouseEvent, uri: string) => void) | undefined;
    const ActualWebLinksAddon = webLinksModule.WebLinksAddon;
    vi.spyOn(webLinksModule, "WebLinksAddon").mockImplementation(function (handler) {
      capturedHandler = handler;
      return new ActualWebLinksAddon(handler);
    });
    const openInBrowserSpy = vi
      .spyOn(commands, "openInBrowser")
      .mockResolvedValue({ status: "ok", data: null } as never);
    const windowOpenSpy = vi.spyOn(window, "open");

    render(<TerminalEmulator stream={new TerminalStream()} sessionId={20} />);
    if (!capturedHandler) throw new Error("Missing captured link handler");

    capturedHandler(new MouseEvent("click"), "https://kubernetes.io");
    expect(openInBrowserSpy).toHaveBeenCalledWith("https://kubernetes.io");

    openInBrowserSpy.mockResolvedValueOnce({
      status: "error",
      error: { kind: "InvalidInput", message: "untrusted scheme" },
    } as never);
    capturedHandler(new MouseEvent("click"), "javascript:alert(1)");
    expect(openInBrowserSpy).toHaveBeenCalledWith("javascript:alert(1)");
    expect(windowOpenSpy).not.toHaveBeenCalled();
  });

  it("routes OSC 8 hyperlinks through openInBrowser via linkHandler", () => {
    const instances = captureTerminals();
    const openInBrowserSpy = vi
      .spyOn(commands, "openInBrowser")
      .mockResolvedValue({ status: "ok", data: null } as never);
    const windowOpenSpy = vi.spyOn(window, "open");

    render(<TerminalEmulator stream={new TerminalStream()} sessionId={21} />);
    const handler = instances[0]?.options.linkHandler;
    if (!handler) throw new Error("Missing OSC 8 link handler");
    handler.activate(new MouseEvent("click"), "https://example.test/docs", {
      start: { x: 1, y: 1 },
      end: { x: 25, y: 1 },
    });
    expect(openInBrowserSpy).toHaveBeenCalledWith("https://example.test/docs");
    expect(windowOpenSpy).not.toHaveBeenCalled();
  });

  it("cleans up xterm, observers, listeners and the stream subscription on unmount", () => {
    const observers: { disconnect: ReturnType<typeof vi.fn> }[] = [];
    vi.stubGlobal(
      "ResizeObserver",
      class {
        disconnect = vi.fn();
        observe = vi.fn();
        constructor() {
          observers.push(this);
        }
      },
    );
    const disposeSpy = vi.spyOn(Terminal.prototype, "dispose");
    const writeSpy = vi.spyOn(Terminal.prototype, "write");
    const removeListenerSpy = vi.spyOn(window, "removeEventListener");
    const stream = new TerminalStream();

    const { unmount } = render(<TerminalEmulator stream={stream} sessionId={30} />);
    expect(observers).toHaveLength(1);

    unmount();
    expect(disposeSpy).toHaveBeenCalledTimes(1);
    expect(observers[0]?.disconnect).toHaveBeenCalledTimes(1);
    expect(removeListenerSpy).toHaveBeenCalledWith("resize", expect.any(Function));

    writeSpy.mockClear();
    stream.write("after unmount");
    expect(writeSpy).not.toHaveBeenCalled();
    vi.unstubAllGlobals();
  });
});
