#!/usr/bin/env bash
set -uo pipefail
cd ~/bench
PY_B64=$(base64 -w0 <<'PY'
from fastapi import FastAPI
app = FastAPI()
@app.get("/health")
def h():
    return {"ok": True}
PY
)
JS_B64=$(base64 -w0 <<'JS'
const e=require("express");const a=e();a.get("/health",(_,r)=>r.send("ok"));a.listen(8080,"0.0.0.0");
JS
)
./build_rootfs_ro.sh tiny-http alpine:3.19 'apk add --no-cache darkhttpd && mkdir -p /www && echo ok > /www/health' 'darkhttpd /www --port 8080' 128
./build_rootfs_ro.sh light-python python:3.11-slim "pip install --no-cache-dir fastapi uvicorn >/tmp/pip.log 2>&1 && mkdir -p /app && echo $PY_B64 | base64 -d > /app/app.py" 'cd /app && python -m uvicorn app:app --host 0.0.0.0 --port 8080' 1024
./build_rootfs_ro.sh light-node node:20-slim "mkdir -p /app && cd /app && npm init -y >/tmp/npm.log 2>&1 && npm install express >>/tmp/npm.log 2>&1 && echo $JS_B64 | base64 -d > /app/app.js" 'cd /app && node app.js' 1024
echo "### RO-BUILDS-DONE"; ls -la ~/bench/*.ext4
