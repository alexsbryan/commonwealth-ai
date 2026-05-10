#!/bin/sh
# Commonwealth installer
# Usage: curl -sSf https://commonwealth.dev/install.sh | sh
#
# Detects platform, downloads the appropriate binary, and installs to /usr/local/bin.

set -e

REPO="commonwealth-rs/commonwealth"
INSTALL_DIR="/usr/local/bin"
BINARY_NAME="commonwealth"

main() {
    echo "Commonwealth Installer"
    echo "======================"
    echo

    # Detect platform.
    OS="$(uname -s)"
    ARCH="$(uname -m)"

    case "$OS" in
        Linux)
            case "$ARCH" in
                x86_64) TARGET="x86_64-unknown-linux-musl" ;;
                aarch64) TARGET="aarch64-unknown-linux-musl" ;;
                *) error "Unsupported architecture: $ARCH" ;;
            esac
            ;;
        Darwin)
            case "$ARCH" in
                arm64) TARGET="aarch64-apple-darwin" ;;
                x86_64) TARGET="x86_64-apple-darwin" ;;
                *) error "Unsupported architecture: $ARCH" ;;
            esac
            ;;
        *)
            error "Unsupported OS: $OS. For Windows, use WSL2 or install manually."
            ;;
    esac

    echo "Detected platform: $OS $ARCH ($TARGET)"

    # Get latest release version.
    VERSION=$(get_latest_version)
    if [ -z "$VERSION" ]; then
        error "Could not determine latest version. Check https://github.com/$REPO/releases"
    fi
    echo "Latest version: $VERSION"

    # Download binary.
    DOWNLOAD_URL="https://github.com/$REPO/releases/download/$VERSION/${BINARY_NAME}-${TARGET}.tar.gz"
    TMPDIR=$(mktemp -d)
    ARCHIVE="$TMPDIR/${BINARY_NAME}.tar.gz"

    echo "Downloading from $DOWNLOAD_URL..."
    if command -v curl > /dev/null 2>&1; then
        curl -sSfL "$DOWNLOAD_URL" -o "$ARCHIVE"
    elif command -v wget > /dev/null 2>&1; then
        wget -q "$DOWNLOAD_URL" -O "$ARCHIVE"
    else
        error "Neither curl nor wget found. Install one and try again."
    fi

    # Extract and install.
    echo "Extracting..."
    tar -xzf "$ARCHIVE" -C "$TMPDIR"

    echo "Installing to $INSTALL_DIR/$BINARY_NAME..."
    if [ -w "$INSTALL_DIR" ]; then
        mv "$TMPDIR/$BINARY_NAME" "$INSTALL_DIR/$BINARY_NAME"
    else
        sudo mv "$TMPDIR/$BINARY_NAME" "$INSTALL_DIR/$BINARY_NAME"
    fi
    chmod +x "$INSTALL_DIR/$BINARY_NAME"

    # Clean up.
    rm -rf "$TMPDIR"

    echo
    echo "Commonwealth $VERSION installed successfully!"
    echo
    echo "Get started:"
    echo "  commonwealth init --name \"My Mesh\"    # Create a mesh"
    echo "  commonwealth --help                    # See all commands"
    echo
}

get_latest_version() {
    if command -v curl > /dev/null 2>&1; then
        curl -sSf "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null \
            | grep '"tag_name"' \
            | head -1 \
            | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/'
    elif command -v wget > /dev/null 2>&1; then
        wget -qO- "https://api.github.com/repos/$REPO/releases/latest" 2>/dev/null \
            | grep '"tag_name"' \
            | head -1 \
            | sed 's/.*"tag_name": *"\([^"]*\)".*/\1/'
    fi
}

error() {
    echo "Error: $1" >&2
    exit 1
}

main "$@"
