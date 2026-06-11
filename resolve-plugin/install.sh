#!/bin/sh
# Install the RoughCut AI Draft script into DaVinci Resolve's Scripts menu
# (macOS). Re-run after updates; Resolve picks it up on next launch (or
# Workspace ▸ Scripts ▸ Refresh).
set -e
DEST="$HOME/Library/Application Support/Blackmagic Design/DaVinci Resolve/Fusion/Scripts/Utility"
mkdir -p "$DEST"
cp "$(dirname "$0")/RoughCut AI Draft.py" "$(dirname "$0")/RoughCut Import Cut.py" "$DEST/"
echo "Installed into: $DEST"
echo "In Resolve: Workspace ▸ Scripts ▸ Utility ▸ RoughCut AI Draft / RoughCut Import Cut"
