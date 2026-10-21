#!/bin/sh
# Starts fcgiwrap (as www-data, the NGINX worker user) and NGINX in the foreground.
set -eu

mkdir -p /run/fcgiwrap
chown www-data:www-data /run/fcgiwrap
# -f: the script's stderr (SCB traces) goes to NGINX, which logs it to the
# container's stderr (`docker compose logs`).
setpriv --reuid=www-data --regid=www-data --init-groups \
    /usr/sbin/fcgiwrap -f -c 4 -s unix:/run/fcgiwrap/fcgiwrap.sock &

exec nginx -g 'daemon off;'
