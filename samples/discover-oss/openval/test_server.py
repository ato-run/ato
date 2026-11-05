"""Integration test against the actual upstream engine and HTTP handlers."""

import importlib.util
import json
import os
import threading
from http.client import HTTPConnection
from http.server import ThreadingHTTPServer
from pathlib import Path
from urllib.error import HTTPError
from urllib.request import Request, urlopen

import pytest


@pytest.fixture(scope="module")
def service():
    root = Path(os.environ["OPENVAL_ROOT"]).resolve()
    spec = importlib.util.spec_from_file_location("openval_adapter", Path(__file__).with_name("server.py"))
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    server = ThreadingHTTPServer(("127.0.0.1", 0), module.Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    yield f"http://127.0.0.1:{server.server_port}", root
    server.shutdown()
    server.server_close()
    thread.join()


def post(base, path, body, content_type):
    request = Request(base + path, data=body, headers={"Content-Type": content_type}, method="POST")
    with urlopen(request, timeout=30) as response:
        return response.status, response.read()


def test_real_workbook_roundtrip_and_cashflow(service):
    base, root = service
    with urlopen(base + "/", timeout=30) as response:
        assert response.status == 200
        assert b"<html" in response.read()
    original = (root / "docs" / "sample_workbook.xlsx").read_bytes()
    status, data = post(base, "/api/parse_workbook", original, "application/octet-stream")
    assert status == 200
    payload = json.loads(data)
    status, data = post(base, "/api/cashflow", json.dumps(payload).encode(), "application/json")
    report = json.loads(data)
    assert status == 200
    assert len(report["years"]) == 5
    assert len(report["rows"]) > 0
    status, workbook = post(base, "/api/export_workbook", json.dumps(payload).encode(), "application/json")
    assert status == 200 and workbook.startswith(b"PK")
    status, data = post(base, "/api/parse_workbook", workbook, "application/octet-stream")
    assert status == 200 and json.loads(data)["name"] == payload["name"]


def test_rejects_invalid_and_oversized_requests(service):
    base, _ = service
    with pytest.raises(HTTPError) as invalid:
        post(base, "/api/cashflow", b"{", "application/json")
    assert invalid.value.code == 400
    with pytest.raises(HTTPError) as unknown:
        post(base, "/api/unknown", b"{}", "application/json")
    assert unknown.value.code == 404
    connection = HTTPConnection("127.0.0.1", int(base.rsplit(":", 1)[1]), timeout=30)
    connection.putrequest("POST", "/api/cashflow")
    connection.putheader("Content-Length", str(10 * 1024 * 1024 + 1))
    connection.endheaders()
    assert connection.getresponse().status == 413
    connection.close()
