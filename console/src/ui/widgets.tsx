/**
 * Small interactive building blocks: stroke icons, icon buttons, an info tip
 * for technical explanations, a disclosure menu for secondary actions, a state
 * dot and a keyboard-operable splitter.
 */
import {
  type CSSProperties,
  useEffect,
  useId,
  useRef,
  useState,
  type KeyboardEvent,
  type PointerEvent,
  type ReactNode,
} from 'react';

const PATHS = {
  search: 'M11 4.5a6.5 6.5 0 1 0 0 13 6.5 6.5 0 0 0 0-13zM16 16l4.5 4.5',
  reload: 'M20 11a8 8 0 1 0-2.3 5.7M20 4v7h-7',
  plus: 'M12 5v14M5 12h14',
  copy: 'M8 8h12v12H8zM16 8V4H4v12h4',
  close: 'M6 6l12 12M18 6L6 18',
  info: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 11v5M12 8h.01',
  sun: 'M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8zM12 2v2M12 20v2M4.9 4.9l1.4 1.4M17.7 17.7l1.4 1.4M2 12h2M20 12h2M4.9 19.1l1.4-1.4M17.7 6.3l1.4-1.4',
  moon: 'M20 14.5A8 8 0 0 1 9.5 4a8 8 0 1 0 10.5 10.5z',
  chevron: 'M6 9l6 6 6-6',
  graph:
    'M6 3.5a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5zM18 5.5a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5zM10 15.5a2.5 2.5 0 1 0 0 5 2.5 2.5 0 0 0 0-5zM8.3 6.4l7.4.9M7 8.3l2.2 7.4M16.6 10.1l-5 6',
} as const;

/** Name of a built-in icon. */
export type IconName = keyof typeof PATHS | 'more';

/** Decorative stroke icon (hidden from assistive technology). */
export function Icon({ name, size = 16 }: { name: IconName; size?: number }) {
  if (name === 'more') {
    return (
      <svg width={size} height={size} viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
        <circle cx="5" cy="12" r="1.8" />
        <circle cx="12" cy="12" r="1.8" />
        <circle cx="19" cy="12" r="1.8" />
      </svg>
    );
  }
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d={PATHS[name]} />
    </svg>
  );
}

/** A square button showing only an icon; `label` is its accessible name. */
export function IconButton({
  icon,
  label,
  onClick,
  disabled,
}: {
  icon: IconName;
  label: string;
  onClick: () => void;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      className="icon-button"
      aria-label={label}
      title={label}
      onClick={onClick}
      disabled={disabled}
    >
      <Icon name={icon} />
    </button>
  );
}

/**
 * An "i" button that reveals a short explanation on hover, focus or click.
 * Technical details stay available without occupying the screen.
 */
export function InfoTip({ label, children }: { label: string; children: ReactNode }) {
  const [open, setOpen] = useState(false);
  const id = useId();
  return (
    <span className="infotip" data-open={open || undefined}>
      <button
        type="button"
        className="icon-button small"
        aria-label={label}
        aria-describedby={id}
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
        onBlur={() => setOpen(false)}
        onKeyDown={(event) => {
          if (event.key === 'Escape') setOpen(false);
        }}
      >
        <Icon name="info" size={14} />
      </button>
      <span id={id} role="tooltip" className="infotip-body">
        {children}
      </span>
    </span>
  );
}

/** One entry of a {@link Menu}. */
export interface MenuItem {
  /** Identifier passed to `onSelect`. */
  id: string;
  label: string;
  danger?: boolean;
  disabled?: boolean;
}

/**
 * A button that opens a list of secondary actions. The list is positioned
 * against the viewport, so scrolling containers do not clip it; it closes on
 * scroll, Escape, Tab or a click elsewhere.
 */
export function Menu({
  label,
  items,
  onSelect,
}: {
  label: string;
  items: MenuItem[];
  onSelect: (id: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const [position, setPosition] = useState<CSSProperties>({});
  const root = useRef<HTMLSpanElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const anchor = useRef({ top: 0, left: 0 });
  const id = useId();

  useEffect(() => {
    if (!open) return;
    root.current?.querySelector<HTMLButtonElement>('[role="menuitem"]:not([disabled])')?.focus();
    const onDown = (event: MouseEvent) => {
      if (!root.current?.contains(event.target as Node)) setOpen(false);
    };
    // Close only when the button actually moved: focusing an item may scroll
    // its container without displacing the menu's anchor.
    const onScroll = () => {
      const rect = button.current?.getBoundingClientRect();
      if (
        !rect ||
        Math.abs(rect.top - anchor.current.top) > 2 ||
        Math.abs(rect.left - anchor.current.left) > 2
      ) {
        setOpen(false);
      }
    };
    document.addEventListener('mousedown', onDown);
    window.addEventListener('scroll', onScroll, true);
    window.addEventListener('resize', onScroll);
    return () => {
      document.removeEventListener('mousedown', onDown);
      window.removeEventListener('scroll', onScroll, true);
      window.removeEventListener('resize', onScroll);
    };
  }, [open]);

  const toggle = () => {
    const rect = button.current?.getBoundingClientRect();
    if (!open && rect) {
      anchor.current = { top: rect.top, left: rect.left };
      const right = window.innerWidth - rect.right;
      const below = window.innerHeight - rect.bottom;
      setPosition(
        below < 40 + 36 * items.length
          ? { right, bottom: window.innerHeight - rect.top + 4 }
          : { right, top: rect.bottom + 4 },
      );
    }
    setOpen((o) => !o);
  };

  const close = () => {
    setOpen(false);
    button.current?.focus();
  };

  const onKey = (event: KeyboardEvent<HTMLDivElement>) => {
    const entries = Array.from(
      root.current?.querySelectorAll<HTMLButtonElement>('[role="menuitem"]:not([disabled])') ?? [],
    );
    const index = entries.indexOf(document.activeElement as HTMLButtonElement);
    if (event.key === 'Escape' || event.key === 'Tab') {
      if (event.key === 'Escape') event.preventDefault();
      close();
    } else if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
      event.preventDefault();
      const step = event.key === 'ArrowDown' ? 1 : -1;
      entries[(index + step + entries.length) % entries.length]?.focus();
    }
  };

  return (
    <span className="menu" ref={root}>
      <button
        ref={button}
        type="button"
        className="icon-button"
        aria-label={label}
        title={label}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? id : undefined}
        onClick={toggle}
      >
        <Icon name="more" />
      </button>
      {open && (
        <div
          id={id}
          role="menu"
          aria-label={label}
          className="menu-list"
          style={position}
          onKeyDown={onKey}
        >
          {items.map((item) => (
            <button
              key={item.id}
              type="button"
              role="menuitem"
              className={item.danger ? 'danger' : undefined}
              disabled={item.disabled}
              onClick={() => {
                close();
                onSelect(item.id);
              }}
            >
              {item.label}
            </button>
          ))}
        </div>
      )}
    </span>
  );
}

/** Severity of a {@link StateDot}. */
export type DotTone = 'ok' | 'warning' | 'muted';

/** A colored dot followed by text; hollow for warnings, so color is not the only cue. */
export function StateDot({ tone, children }: { tone: DotTone; children: ReactNode }) {
  return (
    <span className={`state state-${tone}`}>
      <span className="dot" aria-hidden="true" />
      {children}
    </span>
  );
}

/**
 * Vertical splitter between two panes. Dragging reports the pointer position;
 * arrow keys, Home and End report relative steps.
 */
export function Splitter({
  label,
  value,
  min,
  max,
  onResize,
  onStep,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  /** Called with the new width of the pane right of the splitter. */
  onResize: (width: number) => void;
  onStep: (delta: number | 'min' | 'max') => void;
}) {
  const dragging = useRef(false);
  const onPointerDown = (event: PointerEvent<HTMLDivElement>) => {
    dragging.current = true;
    event.currentTarget.setPointerCapture?.(event.pointerId);
    event.preventDefault();
  };
  const onPointerMove = (event: PointerEvent<HTMLDivElement>) => {
    const parent = event.currentTarget.parentElement;
    if (dragging.current && parent) onResize(parent.getBoundingClientRect().right - event.clientX);
  };
  const onPointerUp = (event: PointerEvent<HTMLDivElement>) => {
    dragging.current = false;
    event.currentTarget.releasePointerCapture?.(event.pointerId);
  };
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const step = event.shiftKey ? 64 : 16;
    const delta =
      event.key === 'ArrowLeft'
        ? step
        : event.key === 'ArrowRight'
          ? -step
          : event.key === 'Home'
            ? 'max'
            : event.key === 'End'
              ? 'min'
              : undefined;
    if (delta !== undefined) {
      event.preventDefault();
      onStep(delta);
    }
  };
  return (
    <div
      className="splitter"
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      aria-valuenow={Math.round(value)}
      aria-valuemin={min}
      aria-valuemax={max}
      tabIndex={0}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerUp}
      onKeyDown={onKeyDown}
    />
  );
}
