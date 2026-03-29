#!/bin/bash

set -e

VERSION="v0.1.0-alpha"
REPO="shwetarkadam/clipd"
INSTALL_DIR="/usr/local/bin"

echo "Installing clipd ${VERSION}..."

# Download the zip
curl -L "https://github.com/${REPO}/releases/download/${VERSION}/clipd-macos-${VERSION}.zip" -o /tmp/clipd.zip

# Unzip
unzip -o /tmp/clipd.zip -d /tmp/clipd-install

# Install binaries
chmod +x /tmp/clipd-install/clipd-macos-v0.1.0-alpha/clipd
chmod +x /tmp/clipd-install/clipd-macos-v0.1.0-alpha/clipd-gui
chmod +x /tmp/clipd-install/clipd-macos-v0.1.0-alpha/clipd-ui

sudo mv /tmp/clipd-install/clipd-macos-v0.1.0-alpha/clipd ${INSTALL_DIR}/clipd
sudo mv /tmp/clipd-install/clipd-macos-v0.1.0-alpha/clipd-gui ${INSTALL_DIR}/clipd-gui
sudo mv /tmp/clipd-install/clipd-macos-v0.1.0-alpha/clipd-ui ${INSTALL_DIR}/clipd-ui

# Cleanup
rm -rf /tmp/clipd.zip /tmp/clipd-install

echo "✅ clipd installed! Run: clipd daemon, then in a new tab run clipd-ui"
