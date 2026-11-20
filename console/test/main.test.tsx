import { act, screen, waitFor } from '@testing-library/react';
import { expect, it, vi } from 'vitest';

it('mounts the console into #root and polls the same-origin API', async () => {
  const fetch = vi.fn(async () => new Response('{"status":"ok"}', { status: 200 }));
  vi.stubGlobal('fetch', fetch);
  const root = document.createElement('div');
  root.id = 'root';
  document.body.append(root);
  const { mount } = await import('../src/main');
  expect(await screen.findByRole('heading', { name: 'Knotrel Console' })).toBeInTheDocument();
  await waitFor(() =>
    expect(fetch).toHaveBeenCalledWith('api/health', expect.objectContaining({ method: 'GET' })),
  );
  const second = document.createElement('div');
  document.body.append(second);
  const extra = mount(second);
  act(() => extra.unmount());
  vi.unstubAllGlobals();
});
