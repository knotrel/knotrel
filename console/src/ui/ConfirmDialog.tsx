/**
 * Modal confirmation for destructive actions. The operator must type the exact
 * target identifier before the confirm button is enabled.
 */
import { useEffect, useId, useRef, useState, type KeyboardEvent } from 'react';

/** Properties of {@link ConfirmDialog}. */
export interface ConfirmDialogProps {
  /** Dialog title. */
  title: string;
  /** Consequence description. */
  description: string;
  /** Exact text the operator must type. */
  confirmText: string;
  /** Confirm button label. */
  actionLabel: string;
  onConfirm: () => void;
  onCancel: () => void;
}

/** Accessible modal dialog with a focus trap and Escape to cancel. */
export function ConfirmDialog(props: ConfirmDialogProps) {
  const { title, description, confirmText, actionLabel, onConfirm, onCancel } = props;
  const [typed, setTyped] = useState('');
  const inputRef = useRef<HTMLInputElement>(null);
  const dialogRef = useRef<HTMLDivElement>(null);
  const titleId = useId();
  const descId = useId();
  const inputId = useId();
  const matches = typed === confirmText;

  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    inputRef.current?.focus();
    return () => previous?.focus();
  }, []);

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (event.key === 'Escape') {
      event.preventDefault();
      onCancel();
      return;
    }
    if (event.key !== 'Tab' || !dialogRef.current) {
      return;
    }
    const focusable = Array.from(
      dialogRef.current.querySelectorAll<HTMLElement>('input, button:not([disabled])'),
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
        ref={dialogRef}
        className="dialog"
        role="alertdialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={descId}
        onKeyDown={onKeyDown}
      >
        <h2 id={titleId}>{title}</h2>
        <p id={descId}>{description}</p>
        <form
          onSubmit={(event) => {
            event.preventDefault();
            if (matches) {
              onConfirm();
            }
          }}
        >
          <label htmlFor={inputId}>
            Type <code className="exact">{confirmText}</code> to confirm
          </label>
          <input
            id={inputId}
            ref={inputRef}
            autoComplete="off"
            spellCheck={false}
            value={typed}
            onChange={(event) => setTyped(event.target.value)}
          />
          <div className="actions">
            <button type="button" onClick={onCancel}>
              Cancel
            </button>
            <button type="submit" className="danger" disabled={!matches}>
              {actionLabel}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
