#!/bin/sh
# Lets the API reach exactly one external SMTP server.
#
# The API container is attached only to the internal control network and has
# no route out. This installs a socket-activated forwarder on the host side of
# that network: connections from the control subnet to LISTEN are relayed to
# SMTP_SERVER and nowhere else. TLS stays end to end between the API and the
# mail server. Without the forwarder the API has no outbound path at all.
#
#   install-smtp-egress.sh LISTEN_IPV4:PORT SMTP_SERVER:PORT CONTROL_SUBNET
#   install-smtp-egress.sh --print LISTEN_IPV4:PORT SMTP_SERVER:PORT CONTROL_SUBNET
#   install-smtp-egress.sh --remove
#
# LISTEN is the control bridge's gateway address and the port in
# ATO_MAIL_SMTP_PORT; CONTROL_SUBNET is that network's subnet. Both come from
# `docker network inspect ato-selfhost_control`. Run as root.
set -eu
unit=ato-selfhost-smtp-egress
dir=${ATO_SYSTEMD_UNIT_DIR:-/etc/systemd/system}
proxy=/usr/lib/systemd/systemd-socket-proxyd

usage() {
  sed -n '10,12p' "$0" | cut -c3- >&2
  exit 2
}

octet='(25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9]?[0-9])'
ipv4="$octet\\.$octet\\.$octet\\.$octet"
port='([1-9][0-9]{0,4})'
label='[A-Za-z0-9]([A-Za-z0-9-]{0,61}[A-Za-z0-9])?'

matches() {
  printf '%s\n' "$1" | grep -Eqx -- "$2"
}

# An address that matches the pattern and whose port is at most 65535.
endpoint() {
  matches "$1" "$2:$port" || return 1
  [ "${1##*:}" -le 65535 ]
}

reject() {
  echo "$1" >&2
  exit 2
}

if [ "${1:-}" = --remove ]; then
  [ "$#" -eq 1 ] || usage
  systemctl disable --now "$unit.socket" "$unit.service" 2>/dev/null || true
  rm -f -- "$dir/$unit.socket" "$dir/$unit.service"
  systemctl daemon-reload
  exit 0
fi

print=false
if [ "${1:-}" = --print ]; then
  print=true
  shift
fi
[ "$#" -eq 3 ] || usage
listen=$1
server=$2
subnet=$3

endpoint "$listen" "$ipv4" || reject 'LISTEN must be IPV4:PORT'
case "$listen" in
  0.* | 127.*) reject 'LISTEN must be the control bridge gateway, not a wildcard or loopback address' ;;
esac
endpoint "$server" "($ipv4|$label(\\.$label)*)" || reject 'SMTP_SERVER must be HOSTNAME:PORT or IPV4:PORT'
matches "$subnet" "$ipv4/([89]|[12][0-9]|30)" ||
  reject 'CONTROL_SUBNET must be an IPv4 subnet such as 172.18.0.0/16'

socket_unit() {
  cat <<UNIT
[Unit]
Description=Ato API outbound SMTP: $listen forwards to $server only

[Socket]
ListenStream=$listen
# The bridge address exists only once Docker has created the network.
FreeBind=yes
# Only the control network may connect.
IPAddressDeny=any
IPAddressAllow=$subnet

[Install]
WantedBy=sockets.target
UNIT
}

service_unit() {
  cat <<UNIT
[Unit]
Description=Forward the Ato API's SMTP connections to $server
Requires=$unit.socket
After=$unit.socket

[Service]
ExecStart=$proxy --connections-max=16 --exit-idle-time=5min $server
DynamicUser=yes
NoNewPrivileges=yes
ProtectSystem=strict
ProtectHome=yes
PrivateTmp=yes
PrivateDevices=yes
CapabilityBoundingSet=
RestrictAddressFamilies=AF_INET AF_INET6 AF_UNIX
UNIT
}

if "$print"; then
  echo "# $dir/$unit.socket"
  socket_unit
  echo "# $dir/$unit.service"
  service_unit
  exit 0
fi

[ "$(id -u)" -eq 0 ] || {
  echo 'Run as root, or use --print to review the units first.' >&2
  exit 1
}
[ -x "$proxy" ] || {
  echo "$proxy is missing (Debian/Ubuntu: package systemd; others: systemd-socket-proxyd)." >&2
  exit 1
}
umask 022
socket_unit > "$dir/$unit.socket"
service_unit > "$dir/$unit.service"
systemctl daemon-reload
# A changed destination takes effect only in a new forwarder process.
systemctl stop "$unit.service" 2>/dev/null || true
systemctl enable "$unit.socket"
systemctl restart "$unit.socket"
echo "Forwarding $listen (from $subnet) to $server."
