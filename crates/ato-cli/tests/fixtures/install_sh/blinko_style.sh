#!/bin/bash
# Blinko-style install script fixture.
#
# This file is used as a PARSE fixture only — it is never executed.
# The DockerRunScriptImporter extracts docker run intent statically.
#
# Original shape from Blinko's public install guide.
# Credentials here are intentionally obviously fake to prevent misuse.

set -e

echo "Installing Blinko..."

# Create dedicated network
docker network create blinko-network

# Start PostgreSQL
docker run -d \
  --name blinko-postgres \
  --network blinko-network \
  -e POSTGRES_USER=postgres \
  -e POSTGRES_PASSWORD=CHANGE_THIS_PASSWORD \
  -e POSTGRES_DB=blinko \
  -v blinko-pg-data:/var/lib/postgresql/data \
  --restart always \
  postgres:14

# Wait for postgres to be ready
echo "Waiting for postgres..."
sleep 5

# Start Blinko app
docker run -d \
  --name blinko-website \
  --network blinko-network \
  -p 1111:1111 \
  -e DATABASE_URL="postgresql://postgres:CHANGE_THIS_PASSWORD@blinko-postgres:5432/blinko" \
  -e NEXTAUTH_SECRET=CHANGE_THIS_SECRET \
  -e NEXTAUTH_URL=http://0.0.0.0:1111 \
  -v blinko-app-data:/app/.blinko \
  --restart always \
  blinkospace/blinko:latest

echo "Blinko is running at http://localhost:1111"
