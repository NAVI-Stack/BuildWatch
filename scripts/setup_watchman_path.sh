#!/bin/bash
# script/setup_watchman_path.sh
# Adds Watchman to the current user's .bashrc if not already present.

WATCHMAN_BIN="/usr/local/watchman/bin"
BASHRC="$HOME/.bashrc"

# Check if already in PATH
if echo "$PATH" | grep -q "$WATCHMAN_BIN"; then
    echo "✅ Watchman is already in your current session PATH."
else
    export PATH="$WATCHMAN_BIN:$PATH"
    echo "🚀 Added Watchman to current session PATH."
fi

# Check if already in .bashrc
if grep -q "$WATCHMAN_BIN" "$BASHRC"; then
    echo "✅ Watchman path is already in $BASHRC."
else
    echo "" >> "$BASHRC"
    echo "# BuildWatch: Add Watchman to PATH" >> "$BASHRC"
    echo "export PATH=\"$WATCHMAN_BIN:\$PATH\"" >> "$BASHRC"
    echo "✅ Added Watchman path to $BASHRC."
    echo "👉 Please run 'source ~/.bashrc' or restart your terminal to apply changes."
fi

# Verify watchman is now accessible
if command -v watchman >/dev/null 2>&1; then
    echo "✨ Watchman is operational: $(watchman --version)"
else
    echo "⚠️  Watchman not found in PATH yet. Run: source ~/.bashrc"
fi
