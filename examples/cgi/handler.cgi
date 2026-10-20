#!/bin/sh
# fcgiwrap executes this wrapper; it hands the request to the SCB interpreter.
# Adjust paths for your install.
exec /usr/local/bin/scb run /srv/scb/cgi/handler.scb
