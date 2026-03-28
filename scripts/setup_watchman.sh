#!/bin/bash
set -e
WATCHMAN_VERSION="v2026.03.23.00"
ZIP_NAME="watchman-${WATCHMAN_VERSION}-linux.zip"
DOWNLOAD_URL="https://github.com/facebook/watchman/releases/download/${WATCHMAN_VERSION}/${ZIP_NAME}"
echo "--- BuildWatch: Watchman Setup ---"
echo "Checking dependencies (curl, unzip)..."
for cmd in curl unzip; do if ! command -v $cmd &> /dev/null; then echo "Error: $cmd is not installed."; exit 1; fi; done
if [ ! -f "/tmp/$ZIP_NAME" ]; then echo "Downloading Watchman ${WATCHMAN_VERSION}..."; curl -L "$DOWNLOAD_URL" -o "/tmp/$ZIP_NAME"; else echo "Using existing download in /tmp/$ZIP_NAME"; fi
echo "Extracting..."
EXTRACT_DIR="/tmp/watchman-install"
rm -rf "$EXTRACT_DIR"
mkdir -p "$EXTRACT_DIR"
unzip -q "/tmp/$ZIP_NAME" -d "$EXTRACT_DIR"
INSTALL_ROOT=$(find "$EXTRACT_DIR" -maxdepth 1 -type d -name "watchman-*" | head -n 1)
if [ -z "$INSTALL_ROOT" ]; then echo "Error: Could not find extracted watchman directory."; exit 1; fi
echo "Installing to /usr/local/..."
sudo mkdir -p /usr/local/bin /usr/local/var/run/watchman
sudo cp "$INSTALL_ROOT/bin/watchman" /usr/local/bin/
sudo chmod +x /usr/local/bin/watchman
sudo chmod 2777 /usr/local/var/run/watchman
echo "Verifying installation..."
if watchman --version &> /dev/null; then echo "Success! Watchman version: $(watchman --version)"; else echo "Error: Watchman installation failed or not in PATH."; exit 1; fi
echo "--- Setup Complete ---"
