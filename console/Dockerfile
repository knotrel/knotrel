# knotrel-console: the static Knotrel operational console served by an
# unprivileged nginx that forwards an allowlist of /api routes to one Knotrel
# server fixed by KNOTREL_UPSTREAM. Build from the console/ directory:
#   docker build -t knotrel-console:dev console
# Configuration, trust boundary and deployment: docs/console.md.

# Static assets are platform-independent: build them once on the build platform.
FROM --platform=$BUILDPLATFORM node:26.13.1-alpine@sha256:6bd9ab43c0ec0190ca7b2d193906cb8b7edf6e7f4afa43ddeccd6452710bffb5 AS assets
WORKDIR /console
COPY package.json package-lock.json ./
RUN npm ci --ignore-scripts --no-audit --no-fund
COPY index.html tsconfig.json tsconfig.app.json tsconfig.node.json vite.config.ts ./
COPY src ./src
RUN npm run build

# nginxinc/nginx-unprivileged:1.30-alpine (runs as uid 101, listens on 8080).
FROM nginxinc/nginx-unprivileged:1.31-alpine@sha256:b5047b9b76845eac124358373c6852377c52e4b05834a47647e955dd91cb8f1e
ARG VERSION=0.0.0-dev
ARG REVISION=unknown
LABEL org.opencontainers.image.title="knotrel-console" \
      org.opencontainers.image.description="Knotrel operational console (static UI and same-origin API gateway)" \
      org.opencontainers.image.source="https://github.com/knotrel/knotrel" \
      org.opencontainers.image.licenses="Elastic-2.0" \
      org.opencontainers.image.version="${VERSION}" \
      org.opencontainers.image.revision="${REVISION}"
# Remove the stock server block and pages: the image serves exactly the console
# files (identical to the static archive's html/) and the rendered templates.
USER root
RUN rm -f /etc/nginx/conf.d/default.conf && rm -rf /usr/share/nginx/html/*
USER 101
COPY container/nginx.conf /etc/nginx/nginx.conf
COPY container/knotrel/ /etc/nginx/knotrel/
COPY container/templates/ /etc/nginx/templates/
COPY container/entrypoint/15-knotrel-console.sh /docker-entrypoint.d/15-knotrel-console.sh
COPY --from=assets /console/dist/ /usr/share/nginx/html/
ENV NGINX_ENVSUBST_OUTPUT_DIR=/tmp/knotrel-console \
    NGINX_ENVSUBST_FILTER=KNOTREL_ \
    KNOTREL_CONSOLE_SERVER_NAMES="localhost 127.0.0.1 [::1]" \
    KNOTREL_UPSTREAM_TIMEOUT=35s
EXPOSE 8080
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
    CMD ["wget", "-q", "-O", "/dev/null", "http://127.0.0.1:8080/healthz"]
