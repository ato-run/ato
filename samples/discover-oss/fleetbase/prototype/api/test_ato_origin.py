import unittest
from ato_origin import socket_origin


class OriginTest(unittest.TestCase):
    def test_valid_origins(self):
        self.assertEqual(socket_origin('https://cinst-example.stg-app.ato.run'),
                         ('https://cinst-example.stg-app.ato.run', 'cinst-example.stg-app.ato.run:443'))
        self.assertEqual(socket_origin('http://localhost:4200'), ('http://localhost:4200', 'localhost:4200'))

    def test_rejects_non_origins_and_insecure_hosts(self):
        for value in ('', '*', 'https://*.example.com', 'https://example.com/path',
                      'https://user@example.com', 'https://example.com?x=1',
                      'http://example.com', 'https://example.com:0',
                      'https://example.com:443', 'https://EXAMPLE.com',
                      'https://example.com\n', 'https://example.com.evil/../'):
            with self.subTest(value=value), self.assertRaises(ValueError):
                socket_origin(value)


if __name__ == '__main__':
    unittest.main()
