/**
 * Modal confirmation for destructive actions. The operator must type the exact
 * target identifier before the confirm button is enabled.
 */
import { useId, useState } from 'react';
import { Dialog } from './Dialog';

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

/** Accessible modal confirmation with a focus trap and Escape to cancel. */
export function ConfirmDialog(props: ConfirmDialogProps) {
  const { title, description, confirmText, actionLabel, onConfirm, onCancel } = props;
  const [typed, setTyped] = useState('');
  const inputId = useId();
  const matches = typed === confirmText;

  return (
    <Dialog title={title} description={description} role="alertdialog" onClose={onCancel}>
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
    </Dialog>
  );
}
