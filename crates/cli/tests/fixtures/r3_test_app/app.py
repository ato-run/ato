import sys
import urllib.request


def check_conn(url):
    try:
        print(f"Connecting to {url}...", end="")
        urllib.request.urlopen(url, timeout=2)
        print(" OK")
        return True
    except Exception as e:
        print(f" Failed: {e}")
        return False


if check_conn("http://httpbin.org/get") and not check_conn("http://example.com"):
    print("SUCCESS: R3 Network Enforcement worked!")
    sys.exit(0)
else:
    print("FAILURE: Network rules did not apply correctly.")
    sys.exit(1)
