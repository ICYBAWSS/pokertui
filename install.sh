#!/bin/sh
# Install the latest `poker` release on macOS or Linux:
#   curl -fsSL https://raw.githubusercontent.com/ICYBAWSS/pokertui/main/install.sh | sh
# Play once without installing (everything is deleted when you quit):
#   curl -fsSL https://raw.githubusercontent.com/ICYBAWSS/pokertui/main/install.sh | sh -s -- --temp
# Set POKER_INSTALL_DIR to choose where it goes (default ~/.local/bin).
set -eu

REPO="ICYBAWSS/pokertui"
DIR="${POKER_INSTALL_DIR:-$HOME/.local/bin}"

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)                target=aarch64-apple-darwin ;;
  Darwin-x86_64)               target=x86_64-apple-darwin ;;
  Linux-x86_64)                target=x86_64-unknown-linux-gnu ;;
  Linux-aarch64 | Linux-arm64) target=aarch64-unknown-linux-gnu ;;
  *)
    echo "No prebuilt poker for $(uname -s) $(uname -m)." >&2
    echo "Install Rust (https://rustup.rs) and run:" >&2
    echo "  cargo install --git https://github.com/$REPO --locked" >&2
    exit 1
    ;;
esac

url="https://github.com/$REPO/releases/latest/download/poker-$target.tar.gz"
echo "Downloading poker for $target..."
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM
curl -fsSL "$url" -o "$tmp/poker.tar.gz"
tar -xzf "$tmp/poker.tar.gz" -C "$tmp"

if [ "${1:-}" = "--temp" ]; then
  # Run from the temp folder, keep config/history there too, delete it all on exit.
  chmod +x "$tmp/poker"
  POKER_HOME="$tmp/home" "$tmp/poker" </dev/tty || true
  exit 0
fi

mkdir -p "$DIR"
mv "$tmp/poker" "$DIR/poker"
chmod +x "$DIR/poker"

echo "Installed: $DIR/poker"
case ":$PATH:" in
  *":$DIR:"*) echo "Run: poker" ;;
  *)
    echo "$DIR is not on your PATH. Add this to your shell profile, then open a new terminal:"
    echo "  export PATH=\"$DIR:\$PATH\""
    ;;
esac
