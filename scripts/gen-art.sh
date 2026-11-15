#!/usr/bin/env bash
# Regenerate the embedded ASCII art from ascii-magic-art stills.
#
# The npm package is only needed at development time; the Rust binary embeds the
# cropped text with include_str! and never calls Node.
set -euo pipefail
cd "$(dirname "$0")/.."

command -v npx >/dev/null || { echo "npx is required (Node.js)"; exit 1; }
mkdir -p assets/art

echo "· rendering lobby splash (slot-reel-letters)"
npx --yes ascii-magic-art@1.1.0 slot-reel-letters --frame --look=ascii > /tmp/pokertui-lobby.raw
python3 - <<'PY'
rows = [line.rstrip() for line in open('/tmp/pokertui-lobby.raw').read().splitlines()[5:29]]
body = [line[:84].ljust(84) for line in rows]
open('assets/art/lobby.txt', 'w').write('\n'.join(body) + '\n')
print('  → assets/art/lobby.txt (%d rows)' % len(body))
PY

echo "· rendering win overlay (fireworks)"
npx --yes ascii-magic-art@1.1.0 fireworks --frame --look=ascii > /tmp/pokertui-win.raw
python3 - <<'PY'
rows = [line[:100] for line in open('/tmp/pokertui-win.raw').read().splitlines()[14:30]]
open('assets/art/win.txt', 'w').write('\n'.join(rows) + '\n')
print('  → assets/art/win.txt (%d rows)' % len(rows))
PY

echo "done — rebuild to embed the new art"
