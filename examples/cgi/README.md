# CGI example (POC milestone M8)

`handler.scb` answers HTTP requests through the CGI contract: request metadata from `\Env\get()`, the body from `\Io\readAll()`, and headers and body via `echo`. NGINX hands each request to fcgiwrap, which runs `handler.cgi` (`scb run handler.scb`).

## In a browser (Docker)

```sh
docker compose -f examples/cgi/docker/compose.yaml up --build
```

Then open <http://localhost:8080/>. Set `SCB_PORT=9000` to use another port.

- The page has forms for GET and POST requests and links for each case the handler covers: a `Status` header, a throw after or before output, and `exit`.
- `examples/cgi/` is mounted into the container, so edits to `handler.scb` apply on the next request.
- SCB traces from uncaught throws show up in `docker compose -f examples/cgi/docker/compose.yaml logs`.

## Automated check (no Docker, no install)

```sh
examples/cgi/smoke-test.sh
```

This runs NGINX and fcgiwrap as your user from a temporary directory and checks every case; see the comments at the top of the script. Results are in `docs/POC_RESULTS.md`.

## On a regular NGINX install

See `nginx.conf` (server block) and `handler.cgi` (the wrapper fcgiwrap executes).
