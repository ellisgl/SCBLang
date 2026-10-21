#!/usr/bin/env bash
# CGI smoke test (POC milestone M8): runs handler.scb through fcgiwrap behind
# NGINX, as an unprivileged user, and checks the responses.
#
# Needs nginx, fcgiwrap and curl. Everything runs from a temporary directory;
# nothing is installed and no system service is touched.
#
# Environment (all optional):
#   SCB       scb binary            (default: target/release/scb, built if missing)
#   NGINX_BIN     nginx binary      (default: nginx on PATH, then /usr/sbin/nginx)
#   FCGIWRAP_BIN  fcgiwrap binary   (default: fcgiwrap on PATH, then /usr/sbin/fcgiwrap)
#   (Not NGINX: nginx itself reads that variable as a list of inherited sockets.)
#   NGINX_CONF_DIR  directory holding fastcgi_params (default: /etc/nginx)
#   PORT      port NGINX listens on (default: 8080)
#   KEEP=1    keep the temporary directory (logs) after the run
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"

find_bin() { # name, fallback
    command -v "$1" 2>/dev/null || { [ -x "$2" ] && echo "$2"; } || { echo "missing: $1" >&2; exit 2; }
}

if [ -z "${SCB:-}" ]; then
    SCB="$repo/target/release/scb"
    [ -x "$SCB" ] || cargo build --release -p scb-cli --manifest-path "$repo/Cargo.toml"
fi
NGINX_BIN="${NGINX_BIN:-$(find_bin nginx /usr/sbin/nginx)}"
FCGIWRAP_BIN="${FCGIWRAP_BIN:-$(find_bin fcgiwrap /usr/sbin/fcgiwrap)}"
unset NGINX
NGINX_CONF_DIR="${NGINX_CONF_DIR:-/etc/nginx}"
PORT="${PORT:-8080}"

work="$(mktemp -d)"
# Unix socket paths are limited to ~108 bytes, so the socket gets a short path.
sock_dir="$(mktemp -d "${XDG_RUNTIME_DIR:-/tmp}/scb-cgi.XXXXXX")"
sock="$sock_dir/fcgiwrap.sock"
nginx_pid=""
fcgi_pid=""
cleanup() {
    [ -n "$nginx_pid" ] && kill "$nginx_pid" 2>/dev/null || true
    [ -n "$fcgi_pid" ] && kill "$fcgi_pid" 2>/dev/null || true
    wait 2>/dev/null || true
    rm -rf "${sock_dir:?}"
    if [ "${KEEP:-0}" = 1 ]; then echo "logs kept in $work"; else rm -rf "${work:?}"; fi
}
trap cleanup EXIT

# The CGI wrapper fcgiwrap executes (handler.cgi with this checkout's paths).
cat > "$work/handler.cgi" <<EOF
#!/bin/sh
exec "$SCB" run "$here/handler.scb"
EOF
chmod +x "$work/handler.cgi"

mkdir -p "$work/tmp"
cat > "$work/nginx.conf" <<EOF
daemon off;
worker_processes 1;
pid $work/nginx.pid;
error_log $work/error.log info;
events { worker_connections 64; }
http {
    access_log $work/access.log;
    client_body_temp_path $work/tmp/body;
    fastcgi_temp_path $work/tmp/fastcgi;
    proxy_temp_path $work/tmp/proxy;
    uwsgi_temp_path $work/tmp/uwsgi;
    scgi_temp_path $work/tmp/scgi;
    client_max_body_size 4m;
    server {
        listen 127.0.0.1:$PORT;
        location /scb {
            gzip off;
            include $NGINX_CONF_DIR/fastcgi_params;
            fastcgi_pass unix:$sock;
            fastcgi_param SCRIPT_FILENAME $work/handler.cgi;
        }
    }
}
EOF

# -f: the script's stderr goes over FastCGI, so NGINX logs it.
"$FCGIWRAP_BIN" -f -s "unix:$sock" 2> "$work/fcgiwrap.log" &
fcgi_pid=$!
"$NGINX_BIN" -p "$work" -e "$work/error.log" -c "$work/nginx.conf" &
nginx_pid=$!

for _ in $(seq 50); do
    [ -S "$sock" ] && curl -s -o /dev/null "http://127.0.0.1:$PORT/" && break
    sleep 0.1
done

url="http://127.0.0.1:$PORT/scb"
failures=0
n=0

# check NAME CURL_ARGS... -- EXPECTED_STATUS PATTERN...
# Each PATTERN is a fixed string that must appear in the response (headers and body).
check() {
    local name="$1"; shift
    local args=()
    while [ "$1" != "--" ]; do args+=("$1"); shift; done
    shift
    local want="$1"; shift
    n=$((n + 1))
    local resp status
    resp="$(curl -s -i "${args[@]}" | tr -d '\r')"
    # Here-strings, not pipes: `grep -q` / `head` closing early would SIGPIPE
    # the writer, which `pipefail` turns into a failure.
    status="$(sed -n '1s/^[^ ]* \([0-9]*\).*/\1/p' <<< "$resp")"
    local ok=1
    [ "$status" = "$want" ] || ok=0
    for p in "$@"; do
        grep -qF -- "$p" <<< "$resp" || { ok=0; echo "  missing: $p"; }
    done
    if [ $ok = 1 ]; then
        echo "PASS $name ($status)"
    else
        echo "FAIL $name (status $status, want $want)"
        sed 's/^/  | /' <<< "$resp" | cut -c1-200
        failures=$((failures + 1))
    fi
}

# log_has NAME PATTERN: the NGINX error log contains PATTERN (the script's stderr).
log_has() {
    n=$((n + 1))
    if grep -qF -- "$2" "$work/error.log"; then
        echo "PASS $1"
    else
        echo "FAIL $1: error log lacks: $2"
        failures=$((failures + 1))
    fi
}

check "GET with query string" "$url?x=1&y=two" -- 200 \
    "X-Powered-By: SCB" "Content-Type: text/plain; charset=utf-8" "method: GET" "query: x=1&y=two" "body bytes: 0"
check "POST with body" -d 'héllo wörld' "$url" -- 200 \
    "method: POST" "body bytes: 13" "body: héllo wörld"
big="$work/big.txt"
head -c 1048576 /dev/zero | tr '\0' 'a' > "$big"
check "POST with 1 MiB body" --data-binary "@$big" "$url" -- 200 "body bytes: 1048576"
check "Status header from the script" "$url?created" -- 201 "X-Powered-By: SCB" "query: created"
check "throw after headers: headers and body so far" "$url?fail" -- 200 \
    "X-Powered-By: SCB" "query: fail" "body bytes: 0"
log_has "throw after headers: trace in error log" "Uncaught Exception: requested failure"
check "throw before any output" "$url?fail-early" -- 502
log_has "throw before any output: trace in error log" "Uncaught Exception: failed before any output"
check "exit(3) after the body" "$url?exit" -- 200 "query: exit"

echo "$n checks: $((n - failures)) passed, $failures failed"
[ $failures = 0 ]
