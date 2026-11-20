/**
 * Browser entry point: mounts the console against the same-origin `api/` prefix.
 */
import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';
import { ApiClient } from './api/client';
import { createConsoleApi } from './api/endpoints';
import { App } from './ui/App';
import './styles.css';

/** Mounts the console into `element`. Returns the React root for unmounting. */
export function mount(element: HTMLElement) {
  const root = createRoot(element);
  root.render(
    <StrictMode>
      <App api={createConsoleApi(new ApiClient())} />
    </StrictMode>,
  );
  return root;
}

const element = document.getElementById('root');
if (element) {
  mount(element);
}
