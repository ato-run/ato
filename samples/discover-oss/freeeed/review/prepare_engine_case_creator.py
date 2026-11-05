#!/usr/bin/env python3
"""Create a local-only internal-case client from the pinned FreeEed source."""
import argparse
import hashlib
from pathlib import Path

EXPECTED = '931bfd7ff5b709029655365388354609adb442a318f2182d07233a6fbd09d965'


def replace_once(text, old, new):
    if text.count(old) != 1:
        raise ValueError('Unexpected upstream source structure')
    return text.replace(old, new, 1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('source', type=Path)
    parser.add_argument('destination', type=Path)
    args = parser.parse_args()
    data = args.source.read_bytes()
    if hashlib.sha256(data).hexdigest() != EXPECTED:
        raise ValueError('Unexpected upstream case creator')
    if args.destination.exists():
        raise ValueError('Destination must not exist')
    text = data.decode('utf-8')
    text = replace_once(text, 'import java.io.File;',
        'import java.io.File;\nimport java.io.InputStream;\nimport java.net.URI;\nimport java.nio.charset.StandardCharsets;')
    text = replace_once(text, '        HttpClientBuilder httpClientBuilder = HttpClientBuilder.create();',
        '        HttpClientBuilder httpClientBuilder = HttpClientBuilder.create().disableRedirectHandling();')
    old = '''            HttpPost request = new HttpPost(url);
            request.setEntity(new UrlEncodedFormEntity(urlParameters));'''
    new = '''            String token = System.getenv("FREEEED_INTERNAL_CASE_TOKEN");
            URI target = URI.create(url);
            if (token == null || !token.matches("[A-Za-z0-9_-]{32,128}")
                    || !"http".equals(target.getScheme()) || target.getUserInfo() != null
                    || !("localhost".equals(target.getHost()) || "127.0.0.1".equals(target.getHost()))
                    || target.getPort() != 8090 || !"/freeeedui/usercase.html".equals(target.getPath())
                    || target.getQuery() != null || target.getFragment() != null) {
                LOGGER.severe("Internal review case endpoint is unavailable or invalid");
                return false;
            }
            HttpPost request = new HttpPost(target);
            request.setHeader("X-FreeEed-Internal-Token", token);
            request.setEntity(new UrlEncodedFormEntity(urlParameters));'''
    text = replace_once(text, old, new)
    old_result = '''            if (response.getStatusLine().getStatusCode() != 200 && response.getStatusLine().getStatusCode() != 302) {
                LOGGER.severe("Invalid Response: " + response.getStatusLine().getStatusCode());
                return false;
            }

            return true;'''
    new_result = '''            if (response.getStatusLine().getStatusCode() != 200 || response.getEntity() == null) {
                LOGGER.severe("Invalid response from internal review endpoint");
                return false;
            }
            try (InputStream body = response.getEntity().getContent()) {
                byte[] bytes = body.readNBytes(4097);
                if (bytes.length > 4096) return false;
                return new String(bytes, StandardCharsets.UTF_8).trim()
                        .matches("Case created successfully with ID: [0-9]+");
            }'''
    text = replace_once(text, old_result, new_result)
    args.destination.parent.mkdir(parents=True, exist_ok=True)
    args.destination.write_text(text)
    print('Local case creator prepared; the token is sent only to the fixed loopback endpoint.')


if __name__ == '__main__':
    main()
