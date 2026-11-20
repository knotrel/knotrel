import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';
import { describeOutcome, OutcomeView, VerdictView } from '../../src/ui/results';
import { failed, ok } from '../helpers';

describe('result rendering', () => {
  it('renders every verdict with icon-and-text status and plain-text server messages', () => {
    const { container } = render(
      <>
        <VerdictView verdict={{ kind: 'effective', stateVersion: '18446744073709551616' }} />
        <VerdictView verdict={{ kind: 'effective', stateVersion: undefined }} />
        <VerdictView verdict={{ kind: 'no-op', stateVersion: '1' }} />
        <VerdictView verdict={{ kind: 'rejected', code: 'self_loop', reason: '<b>bold</b>' }} />
        <VerdictView verdict={{ kind: 'rejected', code: undefined, reason: 'not delivered' }} />
        <VerdictView verdict={{ kind: 'uncertain', reason: 'no response' }} />
      </>,
    );
    expect(screen.getByText('18446744073709551616')).toBeInTheDocument();
    expect(screen.getByText('No-op')).toBeInTheDocument();
    expect(screen.getByText('<b>bold</b>')).toBeInTheDocument();
    expect(container.querySelector('b')).toBeNull();
    expect(screen.getByText('self_loop')).toBeInTheDocument();
    expect(screen.getByText(/a timeout is not a rollback/)).toBeInTheDocument();
  });

  it('describes outcomes and renders query results', () => {
    expect(describeOutcome({ result: 'changed', changed: true })).toBe('Changed');
    expect(describeOutcome({ result: 'changed', changed: false })).toBe('No change');
    expect(
      describeOutcome({
        result: 'edge_state',
        registered: false,
        enabled: false,
        effective: false,
      }),
    ).toBe('Not registered');
    expect(describeOutcome({ result: 'node_state', registered: false, active: false })).toBe(
      'Not registered',
    );
    render(<OutcomeView result={ok({ result: 'connected' as const, connected: false })} />);
    expect(screen.getByText('Not connected')).toBeInTheDocument();
    render(<OutcomeView result={failed({ kind: 'http', status: 500 })} />);
    expect(screen.getByText('HTTP 500')).toBeInTheDocument();
  });
});
