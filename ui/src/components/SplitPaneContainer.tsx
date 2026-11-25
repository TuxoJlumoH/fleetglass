import React, { Children, useEffect, useRef, useState } from "react";
import { type PaneId, useLayoutStore, type SplitMode } from "../stores/layout";

export interface SplitPaneContainerProps {
  primary?: React.ReactNode;
  secondary?: React.ReactNode;
  children?: React.ReactNode;
  splitMode?: SplitMode;
  defaultRatio?: number;
  ratio?: number;
  onRatioChange?: (ratio: number) => void;
  onToggleSplit?: () => void;
  onSplitModeChange?: (mode: SplitMode) => void;
  className?: string;
}

export function SplitPaneContainer({
  primary,
  secondary,
  children,
  splitMode: propSplitMode,
  defaultRatio = 50,
  ratio: propRatio,
  onRatioChange,
  onToggleSplit,
  onSplitModeChange,
  className,
}: SplitPaneContainerProps) {
  const storeSplitMode = useLayoutStore((s) => s.splitMode);
  const toggleStoreSplit = useLayoutStore((s) => s.toggleSplit);
  const toggleStoreOrientation = useLayoutStore((s) => s.toggleSplitOrientation);
  const setStoreSplitMode = useLayoutStore((s) => s.setSplitMode);
  const activePane = useLayoutStore((s) => s.activePane);
  const setActivePane = useLayoutStore((s) => s.setActivePane);

  const activeSplitMode = propSplitMode ?? storeSplitMode;

  const [internalRatio, setInternalRatio] = useState<number>(propRatio ?? defaultRatio);
  const currentRatio = propRatio ?? internalRatio;

  const containerRef = useRef<HTMLDivElement>(null);
  const isDraggingRef = useRef(false);

  const childArray = Children.toArray(children);
  const primaryContent = primary ?? childArray[0] ?? null;
  const secondaryContent = secondary ?? childArray[1] ?? null;

  const updateRatio = (nextRatio: number) => {
    const clamped = Math.min(90, Math.max(10, Math.round(nextRatio)));
    setInternalRatio(clamped);
    onRatioChange?.(clamped);
  };

  // The only Ctrl+\ / Cmd+\ (toggle) and Ctrl+Shift+\ (orientation) listener in the app.
  // `code` is checked too because Shift turns the key into "|" on US layouts.
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (!(e.ctrlKey || e.metaKey) || e.altKey) return;
      if (e.key !== "\\" && e.code !== "Backslash") return;
      e.preventDefault();
      if (e.shiftKey) {
        if (activeSplitMode === "none") return;
        if (onSplitModeChange) {
          onSplitModeChange(activeSplitMode === "horizontal" ? "vertical" : "horizontal");
        } else {
          toggleStoreOrientation();
        }
        return;
      }
      if (onToggleSplit) {
        onToggleSplit();
      } else if (onSplitModeChange) {
        onSplitModeChange(activeSplitMode === "none" ? "horizontal" : "none");
      } else {
        toggleStoreSplit();
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [onToggleSplit, onSplitModeChange, activeSplitMode, toggleStoreSplit, toggleStoreOrientation]);

  // Mouse drag handler for resizing splitter
  const handleMouseDown = (e: React.MouseEvent) => {
    e.preventDefault();
    isDraggingRef.current = true;

    const handleMouseMove = (moveEvent: MouseEvent) => {
      if (!isDraggingRef.current || !containerRef.current) return;
      const rect = containerRef.current.getBoundingClientRect();
      if (activeSplitMode === "horizontal") {
        const offset = moveEvent.clientX - rect.left;
        const percentage = (offset / rect.width) * 100;
        updateRatio(percentage);
      } else if (activeSplitMode === "vertical") {
        const offset = moveEvent.clientY - rect.top;
        const percentage = (offset / rect.height) * 100;
        updateRatio(percentage);
      }
    };

    const handleMouseUp = () => {
      isDraggingRef.current = false;
      window.removeEventListener("mousemove", handleMouseMove);
      window.removeEventListener("mouseup", handleMouseUp);
    };

    window.addEventListener("mousemove", handleMouseMove);
    window.addEventListener("mouseup", handleMouseUp);
  };

  // Separator keyboard accessibility
  const handleKeyDownSeparator = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowLeft" || e.key === "ArrowUp") {
      e.preventDefault();
      updateRatio(currentRatio - 5);
    } else if (e.key === "ArrowRight" || e.key === "ArrowDown") {
      e.preventDefault();
      updateRatio(currentRatio + 5);
    } else if (e.key === "Home") {
      e.preventDefault();
      updateRatio(10);
    } else if (e.key === "End") {
      e.preventDefault();
      updateRatio(90);
    } else if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      updateRatio(50);
    }
  };

  const handleModeChange = (mode: SplitMode) => {
    if (onSplitModeChange) {
      onSplitModeChange(mode);
    } else {
      setStoreSplitMode(mode);
    }
  };

  if (activeSplitMode === "none") {
    return (
      <div
        className={`split-pane-container split-pane-container--none ${className ?? ""}`}
        data-testid="split-pane-container"
        data-split-mode="none"
      >
        <div data-testid="split-pane-primary" className="split-pane split-pane--primary">
          {primaryContent}
        </div>
      </div>
    );
  }

  const isHorizontal = activeSplitMode === "horizontal";
  // Focus (keyboard or click) anywhere in a pane makes it the active one.
  const paneProps = (pane: PaneId) => ({
    "data-testid": `split-pane-${pane}`,
    "data-pane-active": activePane === pane,
    className: `split-pane split-pane--${pane}${activePane === pane ? " split-pane--active" : ""}`,
    tabIndex: -1,
    onFocusCapture: () => {
      setActivePane(pane);
    },
    onMouseDownCapture: () => {
      setActivePane(pane);
    },
  });

  return (
    <div
      ref={containerRef}
      className={`split-pane-container split-pane-container--${activeSplitMode} ${className ?? ""}`}
      data-testid="split-pane-container"
      data-split-mode={activeSplitMode}
      data-active-pane={activePane}
      style={{
        display: "grid",
        gridTemplateColumns: isHorizontal
          ? `${String(currentRatio)}% 6px minmax(0, 1fr)`
          : undefined,
        gridTemplateRows: !isHorizontal
          ? `${String(currentRatio)}% 6px minmax(0, 1fr)`
          : undefined,
        height: "100%",
        width: "100%",
      }}
    >
      <div {...paneProps("primary")}>{primaryContent}</div>

      <div
        role="separator"
        tabIndex={0}
        aria-label="Split pane resize handle"
        aria-orientation={isHorizontal ? "vertical" : "horizontal"}
        aria-valuenow={currentRatio}
        aria-valuemin={10}
        aria-valuemax={90}
        data-testid="split-pane-divider"
        className={`split-pane-divider split-pane-divider--${isHorizontal ? "vertical" : "horizontal"}`}
        onMouseDown={handleMouseDown}
        onKeyDown={handleKeyDownSeparator}
      />

      <div {...paneProps("secondary")}>
        <div className="split-pane__toolbar">
          <div className="split-pane__controls">
            <button
              type="button"
              className={`split-pane__btn ${isHorizontal ? "split-pane__btn--active" : ""}`}
              aria-label="Split horizontally"
              title="Split horizontally"
              onClick={() => {
                handleModeChange("horizontal");
              }}
            >
              Side-by-Side
            </button>
            <button
              type="button"
              className={`split-pane__btn ${!isHorizontal ? "split-pane__btn--active" : ""}`}
              aria-label="Split vertically"
              title="Split vertically"
              onClick={() => {
                handleModeChange("vertical");
              }}
            >
              Stacked
            </button>
            <button
              type="button"
              className="split-pane__btn"
              aria-label="Reset 50/50"
              title="Reset 50/50"
              onClick={() => {
                updateRatio(50);
              }}
            >
              50/50
            </button>
            <button
              type="button"
              className="split-pane__btn split-pane__btn--close"
              aria-label="Close split"
              title="Close split pane"
              onClick={() => {
                handleModeChange("none");
              }}
            >
              ×
            </button>
          </div>
        </div>
        <div className="split-pane__content">{secondaryContent}</div>
      </div>
    </div>
  );
}
