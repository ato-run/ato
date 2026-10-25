# Third-party component record

This is an inventory, not legal advice. The source distributions and image
license files remain authoritative.

| Component | Version/pin | License |
| --- | --- | --- |
| Chatwoot Community Edition | `v4.18.0` | MIT |
| Redis | `8.2.1-alpine` | BSD-3-Clause |
| PostgreSQL | `17.11-r0` | PostgreSQL |
| pgvector | `0.6.2-r1` | PostgreSQL |
| aiohttp | `3.14.3` | Apache-2.0 AND MIT |
| google-api-python-client | `2.200.0` | Apache-2.0 |
| google-auth-oauthlib | `1.4.1` | Apache-2.0 |
| cryptography | `50.0.1` | Apache-2.0 OR BSD-3-Clause |
| PySocks | `1.7.1` | BSD-3-Clause |

The upstream Chatwoot container includes `/app/enterprise` under Chatwoot's
separate Enterprise License. `Dockerfile.support` removes that directory from
the derived image, and local verification asserts that it is absent. No paid
module, license key, or Enterprise feature is part of this fixture.

The direct Python requirements are pinned in `bridge/requirements.txt`.
Transitive Python, Ruby and Alpine package notices can be regenerated from the
derived image for a publication review; this file does not replace those
notices.
