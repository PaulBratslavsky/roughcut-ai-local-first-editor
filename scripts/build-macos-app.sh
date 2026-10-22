#!/usr/bin/env bash
# Build RoughCut.app locally, the same way .github/workflows/release.yml does,
# signed so macOS privacy grants survive rebuilds.
#
# macOS ties Screen Recording, camera and mic permissions to the app's code
# signature. An ad-hoc signature changes on every build, which resets those
# grants. When a "RoughCut Local Signing" code-signing identity is in your
# keychain (or APPLE_SIGNING_IDENTITY names another one), the app is signed
# with it, so its signature stays the same and grants persist. Without one, the
# build falls back to ad-hoc signing.
#
#   scripts/build-macos-app.sh            # → target/release/bundle/macos/RoughCut.app
#   scripts/build-macos-app.sh --bundles app,dmg
set -euo pipefail

cd "$(dirname "$0")/../app/src-tauri"

# ggml-metal emits @available checks (___isPlatformVersionAtLeast). rustc links
# with -nodefaultlibs, so clang's runtime has to be passed in explicitly.
export RUSTFLAGS="-Clink-arg=$(clang --print-resource-dir)/lib/darwin/libclang_rt.osx.a"

args=(--bundles app)
[[ " $* " == *" --bundles "* ]] && args=()

identity="${APPLE_SIGNING_IDENTITY:-RoughCut Local Signing}"
if security find-identity -p codesigning | grep -qF "\"$identity\""; then
  export APPLE_SIGNING_IDENTITY="$identity"
  # Keep hardened runtime off, as in ad-hoc builds. Turning it on would need
  # camera/mic entitlements, which only notarized releases require.
  args+=(--config '{"bundle":{"macOS":{"hardenedRuntime":false}}}')
  echo "Signing with \"$identity\"" >&2
else
  echo "No \"$identity\" signing identity found; building ad-hoc (privacy grants reset on every build)." >&2
fi

# ${args[@]+...}: an empty array trips `set -u` in macOS's bash 3.2.
if command -v cargo-tauri >/dev/null; then
  cargo tauri build ${args[@]+"${args[@]}"} "$@"
else
  npx --yes @tauri-apps/cli@^2 build ${args[@]+"${args[@]}"} "$@"
fi
