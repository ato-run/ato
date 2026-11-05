#!/bin/sh
# Build the proxy configuration from the operator-supplied application origin.
set -eu
set -f
umask 077

invalid() { printf '%s\n' 'Invalid or missing application origin' >&2; exit 1; }
origin=${ATO_BINDING_CONSOLE_ORIGIN:-}
case "$origin" in
  https://*) ATO_SCHEME=https; default_port=443; authority=${origin#https://} ;;
  http://*) ATO_SCHEME=http; default_port=80; authority=${origin#http://} ;;
  *) invalid ;;
esac
case "$authority" in */) authority=${authority%/} ;; esac
case "$authority" in
  *:*) host=${authority%:*}; ATO_PORT=${authority##*:}
       case "$ATO_PORT" in ''|*[!0-9]*|0[0-9]*) invalid ;; esac
       [ "${#ATO_PORT}" -le 5 ] && [ "$ATO_PORT" -ge 1 ] && [ "$ATO_PORT" -le 65535 ] || invalid
       [ "$ATO_PORT" -ne "$default_port" ] || invalid ;;
  *) host=$authority; ATO_PORT=$default_port ;;
esac
case "$host" in ''|.*|*.|*..*|*[!a-z0-9.-]*) invalid ;; esac
[ "${#host}" -le 253 ] || invalid
old_ifs=$IFS
IFS=.
# Intentional label splitting; globbing is disabled above.
# shellcheck disable=SC2086
set -- $host
IFS=$old_ifs
for label do
  case "$label" in ''|-*|*-) invalid ;; esac
  [ "${#label}" -le 63 ] || invalid
done
if [ "$ATO_SCHEME" = http ] && [ "$host" != localhost ]; then
  [ "$#" -eq 4 ] && [ "$1" = 127 ] || invalid
  shift
  for octet do
    case "$octet" in ''|*[!0-9]*|0[0-9]*) invalid ;; esac
    [ "${#octet}" -le 3 ] && [ "$octet" -le 255 ] || invalid
  done
fi
ATO_AUTHORITY=$authority
export ATO_AUTHORITY ATO_PORT ATO_SCHEME
# Preserve nginx variables while envsubst expands only the listed bindings.
# shellcheck disable=SC2016
envsubst '${ATO_AUTHORITY} ${ATO_PORT} ${ATO_SCHEME}' </etc/nginx/ato.conf >/tmp/ato.conf
# shellcheck disable=SC2016
envsubst '${ATO_AUTHORITY} ${ATO_PORT} ${ATO_SCHEME}' </etc/nginx/ato-proxy-headers.conf >/tmp/ato-proxy-headers.conf
exec nginx -c /tmp/ato.conf -g 'daemon off;'
