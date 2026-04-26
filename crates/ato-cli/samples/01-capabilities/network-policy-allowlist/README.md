# network-policy-allowlist

This sample declares `[network] egress_allow = ["httpbin.org"]` and fetches `https://httpbin.org/get` at runtime. On first run, ato surfaces an E302 execution-plan consent gate that explicitly lists `httpbin.org` in the network section — the user must approve before the request is made. It demonstrates ato's network policy declaration: egress targets are auditable and consent-gated before any outbound connection occurs.
