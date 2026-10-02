#!/bin/sh
set -eu
umask 077
case "${APP_UID:?}" in *[!0-9]*|'') exit 2 ;; esac
case "${APP_GID:?}" in *[!0-9]*|'') exit 2 ;; esac
install -d -m 2710 -o "$APP_UID" -g "$APP_GID" /run/exetrouter
for key in hmac oauth; do
    [ "$(wc -c < "/run/secrets/${key}_key")" -eq 32 ] || exit 1
    install -m 600 -o "$APP_UID" -g "$APP_GID" "/run/secrets/${key}_key" "/run/exetrouter/$key.key"
done
