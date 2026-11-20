/**
 * Modal dialog frame: traps focus, closes on Escape and returns focus to the
 * element that opened it.
 */
import { useEffect, useId, useRef, type KeyboardEvent, type ReactNode } from 'react';

/** Properties of {@link Dialog}. */
export interface DialogProps {
  title: string;
  /** Short consequence or purpose text, linked as the description. */
  description?: string;
  /** `alertdialog` for destructive confirmations. */
  role?: 'dialog' | 'alertdialog';
  onClose: () => void;
  children: ReactNode;
}

/** Accessible modal dialog. The first input (or button) receives focus. */
export function Dialog({ title, description, role = 'dialog', onClose, children }: DialogProps) {
  const ref = useRef<HTMLDivElement>(null);
  const titleId = useId();
  const descId = useId();

  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    ref.current?.querySelector<HTMLElement>('input, button:not([disabled])')?.focus();
    return () => previous?.focus();
  }, []);

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === 'Escape') {
      event.preventDefault();
      onClose();
      return;
    }
    if (event.key !== 'Tab' || !ref.current) return;
    const focusable = Array.from(
      ref.current.querySelectorAll<HTMLElement>('input, button:not([disabled])'),
    );
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last?.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first?.focus();
    }
  };

  return (
    <div className="backdrop">
      <div
        ref={ref}
        className="dialog"
        role={role}
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={description ? descId : undefined}
        onKeyDown={onKeyDown}
      >
        <h2 id={titleId}>{title}</h2>
        {description && <p id={descId}>{description}</p>}
        {children}
      </div>
    </div>
  );
}
