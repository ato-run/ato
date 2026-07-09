#!/usr/bin/env bash
set -uo pipefail
cd ~/bench
APP_B64=$(base64 -w0 <<'PY'
from fastapi import FastAPI, Request
from fastapi.responses import PlainTextResponse
app = FastAPI()
_v = {"marker": "", "secret": ""}
@app.get("/health")
def h():
    return {"ok": True}
@app.get("/marker", response_class=PlainTextResponse)
def gm():
    return _v["marker"]
@app.post("/marker", response_class=PlainTextResponse)
async def pm(r: Request):
    _v["marker"] = (await r.body()).decode()
    return "ok"
@app.get("/secret", response_class=PlainTextResponse)
def gs():
    return _v["secret"]
@app.post("/secret", response_class=PlainTextResponse)
async def ps(r: Request):
    _v["secret"] = (await r.body()).decode()
    return "ok"
PY
)
./build_rootfs_ro.sh fulltest python:3.11-slim "pip install --no-cache-dir fastapi uvicorn >/tmp/pip.log 2>&1 && mkdir -p /app && echo $APP_B64 | base64 -d > /app/app.py" 'cd /app && python -m uvicorn app:app --host 0.0.0.0 --port 8080' 1024
echo "### FULLTEST-BUILT"
