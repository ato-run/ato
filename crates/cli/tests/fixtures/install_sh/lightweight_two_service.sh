#!/bin/bash
# Lightweight two-service fixture for opt-in real Podman smoke tests.
#
# Uses only small stable images (alpine:3.20) to minimize pull time.
# "backend" sleeps 30s; "frontend" sleeps 3s then exits, ending the test.
#
# This file is used as a PARSE fixture only — it is never executed.

docker network create test-net

docker run -d \
  --name backend \
  --network test-net \
  -e APP_ENV=test \
  alpine:3.20 \
  sh -c "echo backend-started && sleep 30"

docker run -d \
  --name frontend \
  --network test-net \
  -p 19998:19998 \
  -e BACKEND_URL=http://backend:8080 \
  alpine:3.20 \
  sh -c "echo frontend-started && sleep 3"
