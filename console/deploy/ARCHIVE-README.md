# Knotrel Console — static archive

This archive contains the built Knotrel operational console for hosting behind
a web server or gateway you already operate.

| Path                         | Content                                                                 |
| ---------------------------- | ----------------------------------------------------------------------- |
| `html/`                      | The console. Serve this directory as static files.                      |
| `nginx/knotrel-console.conf` | Example same-origin gateway configuration.                              |
| `console-manifest.json`      | Version, source revision, OpenAPI digest and supported server versions. |
| `SHA256SUMS`                 | Checksums of every other file (`sha256sum -c SHA256SUMS`).              |

## Requirements

1. Serve `html/` and, **on the same origin**, forward these requests to one
   Knotrel server (strip the `/api` prefix): `GET /api/health`,
   `GET /api/ready`, `GET /api/v1/info`, `GET /api/v1/stats`,
   `POST /api/v1/operations`, `POST /api/v1/batch`, `GET /api/metrics`,
   `GET /api/v1/nodes`, `GET /api/v1/nodes/{id}`,
   `GET /api/v1/nodes/{id}/edges` and `GET /api/v1/edges`. Forward nothing else:
   feeder routes stay private.
2. Provide TLS and access control at the gateway. The console adds no
   authentication and the Knotrel API has none.
3. Do not let browsers choose the upstream: it is fixed by your configuration.
4. Optionally serve `console-config.json` next to `index.html`, for example
   `{"serverLabel":"knotrel-prod-1"}`, to show which server the gateway uses.

The console uses the relative prefix `api/`, so it also works under a sub-path.
No rebuild is needed per installation.

Full documentation: https://github.com/knotrel/knotrel/blob/main/docs/console.md

Licensed under the Elastic License 2.0 (`LICENSE.md`).
