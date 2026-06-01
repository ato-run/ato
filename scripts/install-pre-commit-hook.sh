#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
hook_path="$repo_root/.git/hooks/pre-commit"
script_path="$repo_root/scripts/pre-commit-quality.sh"

chmod +x "$script_path"

cat > "$hook_path" <<EOF
#!/usr/bin/env bash
exec "$script_path"
EOF

chmod +x "$hook_path"

echo "installed pre-commit hook: $hook_path"
