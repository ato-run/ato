#!/bin/sh
set -eu
mkdir -p dist
cat > dist/run.sh <<'EOF'
#!/bin/sh
set -eu
printf 'build-output-layer-ok\n'
EOF
chmod +x dist/run.sh
