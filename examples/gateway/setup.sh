#!/usr/bin/env bash
# Generates the example's TLS material and credentials into .generated/
# (ignored by git). Existing files are kept; delete .generated/ to start over.
#
#   .generated/tls/ca.crt            trust anchor for clients (its key is deleted
#                                    after signing: nothing else is ever signed)
#   .generated/tls/server.{crt,key}  gateway certificate for localhost
#   .generated/auth/{operators,feeders,probes}  htpasswd files (apr1 hashes)
#   .generated/credentials           user names and passwords (mode 600)
set -euo pipefail
cd "$(dirname "$0")"
out=.generated
mkdir -p "$out/tls" "$out/auth"
chmod 700 "$out"

if [[ ! -s "$out/tls/server.crt" ]]; then
  ca_dir="$(mktemp -d)"
  trap 'rm -rf "$ca_dir"' EXIT
  openssl req -x509 -newkey rsa:3072 -nodes -days 30 -subj "/CN=knotrel-example-ca" \
    -keyout "$ca_dir/ca.key" -out "$out/tls/ca.crt" 2>/dev/null
  openssl req -newkey rsa:3072 -nodes -subj "/CN=localhost" \
    -keyout "$out/tls/server.key" -out "$out/tls/server.csr" 2>/dev/null
  printf 'subjectAltName=DNS:localhost,IP:127.0.0.1\nextendedKeyUsage=serverAuth\n' > "$out/tls/ext.cnf"
  openssl x509 -req -in "$out/tls/server.csr" -CA "$out/tls/ca.crt" -CAkey "$ca_dir/ca.key" \
    -CAcreateserial -days 30 -extfile "$out/tls/ext.cnf" -out "$out/tls/server.crt" 2>/dev/null
  rm -f "$out/tls/server.csr" "$out/tls/ext.cnf" "$out/tls/ca.srl"
  rm -rf "$ca_dir"
  # The gateway runs as uid 101; the directory stays 700 for other host users.
  chmod 644 "$out/tls/server.key" "$out/tls/server.crt" "$out/tls/ca.crt"
  echo "created TLS material in $out/tls"
fi

if [[ ! -s "$out/credentials" ]]; then
  operator_password="$(od -An -N18 -tx1 /dev/urandom | tr -d ' \n')"
  feeder_password="$(od -An -N18 -tx1 /dev/urandom | tr -d ' \n')"
  printf 'operator:%s\n' "$(openssl passwd -apr1 "$operator_password")" > "$out/auth/operators"
  printf 'feeder:%s\n' "$(openssl passwd -apr1 "$feeder_password")" > "$out/auth/feeders"
  cat "$out/auth/operators" "$out/auth/feeders" > "$out/auth/probes"
  chmod 644 "$out/auth/"*
  umask 077
  printf 'operator %s\nfeeder %s\n' "$operator_password" "$feeder_password" > "$out/credentials"
  echo "created credentials in $out/credentials"
fi
