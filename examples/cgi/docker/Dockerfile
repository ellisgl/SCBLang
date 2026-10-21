# NGINX + fcgiwrap + SCB, for trying the CGI example in a browser.
# Built from the repository root (see compose.yaml next to this file).

# ---- build the interpreter ----
FROM rust:1.95-slim-trixie AS build
WORKDIR /src
COPY . .
RUN cargo build --release --locked -p scb-cli

# ---- runtime: Debian's nginx and fcgiwrap ----
FROM debian:trixie-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends nginx fcgiwrap \
    && rm -rf /var/lib/apt/lists/* \
    && rm -f /etc/nginx/sites-enabled/default
COPY --from=build /src/target/release/scb /usr/local/bin/scb
COPY examples/cgi/docker/nginx.conf /etc/nginx/conf.d/scb.conf
COPY examples/cgi/docker/entrypoint.sh /usr/local/bin/scb-entrypoint
# The example itself (compose also bind-mounts it, so edits apply without a rebuild).
COPY examples/cgi/ /srv/scb/cgi/
EXPOSE 80
ENTRYPOINT ["/usr/local/bin/scb-entrypoint"]
