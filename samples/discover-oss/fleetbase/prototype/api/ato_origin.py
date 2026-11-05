"""Validate the one external origin used by the Fleetbase local prototype."""
import ipaddress
import re
from urllib.parse import urlsplit


def socket_origin(value):
    if not isinstance(value, str) or not value or value != value.strip():
        raise ValueError('Application origin is required')
    try:
        parsed = urlsplit(value)
        host = parsed.hostname
        port = parsed.port
    except ValueError as error:
        raise ValueError('Invalid application origin') from error
    if (parsed.scheme not in ('http', 'https') or not host or
            parsed.username is not None or parsed.password is not None or
            parsed.path not in ('', '/') or parsed.query or parsed.fragment or
            not host.isascii() or len(host) > 253 or not re.fullmatch(r'[a-z0-9.-]+', host)):
        raise ValueError('Invalid application origin')
    labels = host.split('.')
    if any(not re.fullmatch(r'[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?', label) for label in labels):
        raise ValueError('Invalid application hostname')
    try:
        address = ipaddress.ip_address(host)
    except ValueError:
        address = None
    if parsed.scheme == 'http' and not (host == 'localhost' or address and address.is_loopback):
        raise ValueError('Application origin requires HTTPS')
    if port is None:
        port = 443 if parsed.scheme == 'https' else 80
    if not 1 <= port <= 65535:
        raise ValueError('Invalid application port')
    canonical = f'{parsed.scheme}://{host}'
    if port != (443 if parsed.scheme == 'https' else 80):
        canonical += f':{port}'
    if value.rstrip('/') != canonical:
        raise ValueError('Application origin must be canonical')
    return canonical, f'{host}:{port}'
