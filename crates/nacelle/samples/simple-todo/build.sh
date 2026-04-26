#!/bin/bash
# Build script for Simple TODO Capsule

set -e

echo "🔨 Building Simple TODO Capsule..."
echo "=================================="

# Change to the script directory
SCRIPT_DIR="$( cd "$( dirname "${BASH_SOURCE[0]}" )" && pwd )"
PROJECT_ROOT="$SCRIPT_DIR/../../../.."

# Build the React application from ato-desktop
echo "📦 Building React + Vite application..."
cd "$PROJECT_ROOT/ato-desktop"
pnpm run build

# Copy dist/todo to samples directory
echo "📋 Copying built application..."
mkdir -p "$SCRIPT_DIR/dist"
if [ -f "$PROJECT_ROOT/ato-desktop/dist/todo.html" ]; then
  cp "$PROJECT_ROOT/ato-desktop/dist/todo.html" "$SCRIPT_DIR/dist/"
  echo "  ✓ Copied todo.html"
fi

# Copy assets if they exist
if [ -d "$PROJECT_ROOT/ato-desktop/dist/assets" ]; then
  cp -r "$PROJECT_ROOT/ato-desktop/dist/assets" "$SCRIPT_DIR/dist/"
  echo "  ✓ Copied assets"
fi

# Verify capsule structure
echo "✅ Verifying capsule structure..."
if [ -f "$SCRIPT_DIR/capsule.toml" ]; then
  echo "  ✓ capsule.toml found"
else
  echo "  ✗ capsule.toml missing"
  exit 1
fi

if [ -f "$SCRIPT_DIR/app.py" ]; then
  echo "  ✓ app.py found"
else
  echo "  ✗ app.py missing"
  exit 1
fi

if [ -d "$SCRIPT_DIR/dist" ] && [ -f "$SCRIPT_DIR/dist/todo.html" ]; then
  echo "  ✓ dist/ directory with built app found"
else
  echo "  ⚠ dist/ may be incomplete (this is ok for dev)"
fi

echo ""
echo "🎉 Build complete!"
echo "=================================="
echo "Next steps:"
echo "  1. Run locally: python3 $SCRIPT_DIR/app.py"
echo "  2. Test: curl http://localhost:8000/api/health"
echo "  3. Package: nacelle package $SCRIPT_DIR/capsule.toml"
echo ""
echo "Or from ato-desktop, run the Vite dev server:"
echo "  cd $PROJECT_ROOT/ato-desktop && pnpm run dev"
echo "  Then visit: http://localhost:5173/todo.html"
