# Fixed SOCKS5 + TLS transport Adapter

This fixture-owned Adapter accepts bytes on one internal TCP listener and
forwards them to one fixed numeric IPv4/port through the Instance's
`ato.tcp-egress@1` SOCKS5 Binding. It authenticates the external TLS endpoint
with an explicit CA and DNS server name.

It deliberately has no SMTP parser, MX/DNS lookup, recipient routing, dynamic
CONNECT target, proxy authentication, or certificate-verification bypass.

Required environment:

- `ATO_BINDING_SMTP_EGRESS=socks5://<broker-ip>:<port>`
- `SMTP_RELAY_TARGET=<public-ip>:<port>`
- `SMTP_RELAY_TLS_SERVER_NAME=<certificate DNS name>`
- `SMTP_RELAY_CA_PEM=<read-only path to PEM CA bundle>`

Optional bounds are `ADAPTER_LISTEN`, `ADAPTER_CONNECT_TIMEOUT_SECS`,
`ADAPTER_IDLE_TIMEOUT_SECS`, and `ADAPTER_MAX_CONNECTIONS`.

