#!/usr/bin/env bash
#
# Install git hooks for Thala
# Symlinks committed hooks into .git/hooks/
# Usage: ./dev/scripts/install-hooks.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/../.." && pwd)"
GIT_HOOKS_DIR="${REPO_ROOT}/.git/hooks"
HOOKS_SOURCE_DIR="${SCRIPT_DIR}/hooks"

if [ ! -d "$GIT_HOOKS_DIR" ]; then
    echo "❌ Error: .git/hooks directory not found at ${GIT_HOOKS_DIR}"
    echo "   Are you running this from the repository root?"
    exit 1
fi

if [ ! -d "$HOOKS_SOURCE_DIR" ]; then
    echo "❌ Error: Hooks source directory not found at ${HOOKS_SOURCE_DIR}"
    exit 1
fi

echo "==> Installing git hooks..."
echo "    Source: ${HOOKS_SOURCE_DIR}"
echo "    Target: ${GIT_HOOKS_DIR}"
echo ""

# Track if any hooks were installed
INSTALLED=0

# Install each hook from the source directory
for hook_file in "$HOOKS_SOURCE_DIR"/*; do
    if [ -f "$hook_file" ]; then
        hook_name=$(basename "$hook_file")
        target_path="${GIT_HOOKS_DIR}/${hook_name}"
        
        # Check if there's an existing hook (not a symlink to our source)
        if [ -f "$target_path" ] && [ ! -L "$target_path" ]; then
            echo "⚠️  Existing hook found: ${hook_name}"
            echo "   Backing up to: ${target_path}.backup"
            mv "$target_path" "${target_path}.backup"
        fi
        
        # Create or update symlink
        if [ -L "$target_path" ]; then
            # Remove existing symlink
            rm "$target_path"
        fi
        
        # Create relative symlink
        rel_path=$(realpath --relative-to="$GIT_HOOKS_DIR" "$hook_file")
        ln -s "$rel_path" "$target_path"
        chmod +x "$hook_file"
        
        echo "✅ Installed: ${hook_name}"
        INSTALLED=$((INSTALLED + 1))
    fi
done

if [ $INSTALLED -eq 0 ]; then
    echo "⚠️  No hooks found in ${HOOKS_SOURCE_DIR}"
else
    echo ""
    echo "✅ Successfully installed ${INSTALLED} hook(s)"
    echo ""
    echo "   Hooks will now run automatically on git operations."
    echo "   To bypass pre-push hooks temporarily, use: git push --no-verify"
fi

echo ""
