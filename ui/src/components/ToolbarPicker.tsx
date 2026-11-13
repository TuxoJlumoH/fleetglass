import { useEffect, useId, useRef, useState, type KeyboardEvent, type ReactNode } from "react";
import type { StatusTone } from "./statusTone";

export interface PickerOption {
  key: string;
  label: string;
  checked: boolean;
  /** Shown right-aligned (e.g. rows per status); omitted when null/undefined. */
  count?: number | null;
  /** Coloured dot before the label (status picker). */
  tone?: StatusTone;
}

export interface ToolbarPickerProps {
  /** Accessible name of the menu (e.g. "Status filter"). */
  name: string;
  /**
   * Visible button content (e.g. "Status: any"); it is also the button's accessible name, so
   * speech users can say what they see.
   */
  label: ReactNode;
  icon: ReactNode;
  options: readonly PickerOption[];
  onToggle: (key: string) => void;
  /** Footer action, e.g. "Any status" / "Show all"; hidden when null. */
  reset?: { label: string; onReset: () => void; disabled: boolean } | null;
  testId?: string;
}

/**
 * Toolbar menu button with checkable items (status and column pickers). Every item is a
 * `menuitemcheckbox` button, so a click anywhere on the row toggles it. Keyboard: Enter, Space
 * or ArrowDown opens the menu and focuses the first item; ArrowUp/ArrowDown/Home/End move;
 * Space or Enter toggles (native button activation, one click per press); Escape closes it and
 * returns focus to the button without reaching the table or the drawer; Tab out or a click
 * outside closes it.
 */
export function ToolbarPicker({
  name,
  label,
  icon,
  options,
  onToggle,
  reset = null,
  testId,
}: ToolbarPickerProps) {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement | null>(null);
  const buttonRef = useRef<HTMLButtonElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const focusFirstRef = useRef(false);
  const menuId = useId();

  const items = (): HTMLElement[] =>
    menuRef.current
      ? Array.from(menuRef.current.querySelectorAll<HTMLElement>("button:not(:disabled)"))
      : [];

  useEffect(() => {
    if (!open) return;
    if (focusFirstRef.current) {
      focusFirstRef.current = false;
      items()[0]?.focus();
    }
    const onPointerDown = (e: MouseEvent) => {
      if (rootRef.current && e.target instanceof Node && !rootRef.current.contains(e.target)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", onPointerDown);
    return () => {
      document.removeEventListener("mousedown", onPointerDown);
    };
  }, [open]);

  const openAndFocus = () => {
    focusFirstRef.current = true;
    setOpen(true);
  };

  const close = (refocus: boolean) => {
    setOpen(false);
    if (refocus) buttonRef.current?.focus();
  };

  const onRootKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    if (e.key === "Escape" && open) {
      // Only the picker closes: the table and the drawer never see this Escape.
      e.preventDefault();
      e.stopPropagation();
      close(true);
    }
  };

  const onButtonKeyDown = (e: KeyboardEvent<HTMLButtonElement>) => {
    // Enter and Space open through the native click (see onClick); ArrowDown opens too.
    if (e.key === "ArrowDown") {
      e.preventDefault();
      e.stopPropagation();
      if (open) items()[0]?.focus();
      else openAndFocus();
    }
  };

  const onMenuKeyDown = (e: KeyboardEvent<HTMLDivElement>) => {
    const list = items();
    const index = list.indexOf(document.activeElement as HTMLElement);
    let next: number | null = null;
    if (e.key === "ArrowDown") next = Math.min(list.length - 1, index + 1);
    else if (e.key === "ArrowUp") next = Math.max(0, index - 1);
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = list.length - 1;
    if (next !== null) {
      e.preventDefault();
      e.stopPropagation();
      list[next]?.focus();
    }
    // Space/Enter are left to the focused button: the browser turns each press into exactly
    // one click, which toggles the item.
  };

  return (
    <div
      ref={rootRef}
      className="tb-picker"
      onKeyDown={onRootKeyDown}
      onBlur={(e) => {
        // Tab out of the picker closes it. A null relatedTarget (click on a non-focusable spot)
        // is left to the outside-mousedown handler.
        const to = e.relatedTarget;
        if (open && to instanceof Node && !rootRef.current?.contains(to)) setOpen(false);
      }}
      {...(testId ? { "data-testid": testId } : {})}
    >
      <button
        ref={buttonRef}
        type="button"
        className="tb-btn"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={menuId}
        onClick={() => {
          if (open) close(false);
          else openAndFocus();
        }}
        onKeyDown={onButtonKeyDown}
      >
        {icon}
        <span className="tb-btn__label">{label}</span>
        <svg className="i tb-chev" viewBox="0 0 24 24" aria-hidden="true">
          <path d="m6 9 6 6 6-6" />
        </svg>
      </button>
      <div
        ref={menuRef}
        id={menuId}
        role="menu"
        aria-label={name}
        className="tb-menu"
        hidden={!open}
        onKeyDown={onMenuKeyDown}
      >
        {options.map((opt) => (
          <button
            key={opt.key}
            type="button"
            role="menuitemcheckbox"
            aria-checked={opt.checked}
            tabIndex={-1}
            className="tb-menu__item"
            onClick={() => {
              onToggle(opt.key);
            }}
          >
            <span className="tb-check" aria-hidden="true">
              <svg viewBox="0 0 16 16">
                <path d="m3.5 8.5 3 3 6-7" />
              </svg>
            </span>
            {opt.tone && <span className={`tb-dot ${opt.tone}`} aria-hidden="true" />}
            <span className="tb-menu__label">{opt.label}</span>
            {opt.count !== undefined && opt.count !== null && (
              <span className="tb-menu__count">{opt.count.toLocaleString("en-US")}</span>
            )}
          </button>
        ))}
        {reset && (
          <button
            type="button"
            role="menuitem"
            tabIndex={-1}
            className="tb-menu__reset"
            disabled={reset.disabled}
            onClick={() => {
              reset.onReset();
            }}
          >
            {reset.label}
          </button>
        )}
      </div>
    </div>
  );
}
