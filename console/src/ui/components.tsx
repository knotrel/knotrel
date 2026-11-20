/**
 * Shared presentational components. All server-provided text is rendered as
 * React text nodes, never as HTML.
 */
import { useId, type ReactNode } from 'react';
import type { Failure } from '../api/endpoints';
import type { VersionMetadata } from '../api/metadata';
import { summarizeFailure } from '../domain/errors';
import { checkNodeId, hasLeadingZeroes } from '../domain/node-id';
import type { Severity } from '../domain/sync';

const ICONS: Record<Severity, string> = {
  ok: '✓',
  progress: '…',
  warning: '!',
  critical: '✕',
  unknown: '?',
};

/** A status indicator that conveys severity by icon and text, not color alone. */
export function StatusBadge({ severity, label }: { severity: Severity; label: string }) {
  return (
    <span className={`badge badge-${severity}`}>
      <span className="badge-icon" aria-hidden="true">
        {ICONS[severity]}
      </span>
      <span>{label}</span>
    </span>
  );
}

/** Explicit marker for a field the server does not expose or did not return. */
export function Unavailable({ reason }: { reason: string }) {
  return <span className="unavailable">Unavailable — {reason}</span>;
}

/** A label/value pair in a definition list. */
export function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="field">
      <dt>{label}</dt>
      <dd>{children}</dd>
    </div>
  );
}

/** Monospace exact value (IDs, revisions, positions). */
export function Exact({ value }: { value: string }) {
  return <code className="exact">{value}</code>;
}

/** Formats a local time with seconds. */
export function formatTime(date: Date): string {
  return date.toLocaleTimeString(undefined, { hour12: false });
}

/** Local time element. */
export function Time({ date }: { date: Date }) {
  return <time dateTime={date.toISOString()}>{formatTime(date)}</time>;
}

/** Formats a browser-measured duration. */
export function formatDuration(ms: number): string {
  return ms < 1000 ? `${Math.round(ms)} ms` : `${(ms / 1000).toFixed(2)} s`;
}

/** Displays a classified request failure. */
export function FailureView({ failure }: { failure: Failure }) {
  const summary = summarizeFailure(failure);
  return (
    <div className="failure" role="alert">
      <StatusBadge severity="critical" label={summary.title} />
      <p>{summary.detail}</p>
      {summary.code !== undefined && (
        <p>
          Code: <Exact value={summary.code} />
        </p>
      )}
      {summary.serverMessage !== undefined && (
        <p>
          Message: <span className="server-text">{summary.serverMessage}</span>
        </p>
      )}
    </div>
  );
}

/** Version metadata captured with one response. */
export function MetadataView({ metadata }: { metadata: VersionMetadata }) {
  const value = (v: string | undefined, reason: string) =>
    v === undefined ? <Unavailable reason={reason} /> : <Exact value={v} />;
  return (
    <dl className="metadata">
      <Field label="Instance">{value(metadata.instanceId, 'header absent')}</Field>
      <Field label="Generation">{value(metadata.generation, 'header absent')}</Field>
      <Field label="State version">{value(metadata.stateVersion, 'header absent')}</Field>
      <Field label="Source position">
        {value(metadata.sourcePosition, 'none (standalone or not yet set)')}
      </Field>
      {metadata.syncStatus !== undefined && (
        <Field label="Sync status">
          <Exact value={metadata.syncStatus} />
        </Field>
      )}
      {metadata.invalid.length > 0 && (
        <Field label="Malformed headers">
          <span className="server-text">{metadata.invalid.join(', ')}</span>
        </Field>
      )}
    </dl>
  );
}

/**
 * A validated node ID input. Reports its value only when valid. The format
 * hint ("unsigned 64-bit decimal ID") is announced to assistive technology but
 * shown only when the value needs attention.
 */
export function NodeIdField({
  label,
  value,
  onChange,
  disabled,
  hideLabel,
  placeholder,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  disabled?: boolean;
  /** Keeps the label for assistive technology only (compact toolbars). */
  hideLabel?: boolean;
  placeholder?: string;
}) {
  const id = useId();
  const check = value === '' ? undefined : checkNodeId(value);
  const hintId = `${id}-hint`;
  const message =
    check && !check.ok
      ? check.message
      : check?.ok && hasLeadingZeroes(value)
        ? 'The server normalizes leading zeroes.'
        : undefined;
  return (
    <div className="node-field">
      <label htmlFor={id} className={hideLabel ? 'visually-hidden' : undefined}>
        {label}
      </label>
      <input
        id={id}
        inputMode="numeric"
        autoComplete="off"
        spellCheck={false}
        value={value}
        disabled={disabled}
        placeholder={placeholder}
        aria-invalid={check !== undefined && !check.ok}
        aria-describedby={hintId}
        onChange={(event) => onChange(event.target.value)}
      />
      <span id={hintId} className={message ? 'hint' : 'visually-hidden'}>
        {message ?? 'Unsigned 64-bit decimal ID.'}
      </span>
    </div>
  );
}

/** True when every value is a valid node ID. */
export function allValid(...values: string[]): boolean {
  return values.every((v) => checkNodeId(v).ok);
}
