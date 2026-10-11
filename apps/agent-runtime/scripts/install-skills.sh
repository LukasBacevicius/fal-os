#!/bin/sh
# Fetch the official agent skills the motion agent loads (as local Claude Code plugins):
#   skills/remotion     ← github.com/remotion-dev/claude-code-plugin (Remotion; plugin.json: MIT)
#   skills/hyperframes  ← github.com/heygen-com/hyperframes, .claude-plugin + skills/ (Apache-2.0)
# They are fetched from upstream at pinned commits, not vendored into this repo (the Remotion
# skills repo carries no LICENSE file; see README → Skills). Re-run to update the pins below.
set -eu
REMOTION_COMMIT=50993d0b202a5be8e3d92b5ae0c8448248e961d1
HYPERFRAMES_COMMIT=8bca7c4417b59b47fca851183a8d47a4565bad0c
DIR="${FALOS_SKILLS_DIR:-$(cd "$(dirname "$0")/.." && pwd)/skills}"
mkdir -p "$DIR"

fetch() { # name repo commit [sparse paths…]
  name=$1 repo=$2 commit=$3
  shift 3
  tmp="$DIR/.$name.tmp"
  rm -rf "$tmp"
  git init -q "$tmp"
  git -C "$tmp" remote add origin "https://github.com/$repo.git"
  if [ $# -gt 0 ]; then
    git -C "$tmp" config core.sparseCheckout true
    git -C "$tmp" sparse-checkout set --no-cone "$@"
  fi
  git -C "$tmp" fetch -q --depth 1 --filter=blob:none origin "$commit"
  git -C "$tmp" checkout -q FETCH_HEAD
  rm -rf "$tmp/.git" "$DIR/$name"
  mv "$tmp" "$DIR/$name"
  echo "$repo@$commit" > "$DIR/$name/SOURCE"
  echo "installed $name skills → $DIR/$name"
}

fetch remotion remotion-dev/claude-code-plugin "$REMOTION_COMMIT"
fetch hyperframes heygen-com/hyperframes "$HYPERFRAMES_COMMIT" "/.claude-plugin/" "/skills/" "/LICENSE"
