#!/usr/bin/env bash
# Build RoughCut.app locally, the same way .github/workflows/release.yml does,
# signed so macOS per-app grants survive rebuilds.
#
# macOS ties per-app grants to the app's code signature, such as access to the
# Keychain item that holds the Hosted-mode API key. An ad-hoc signature changes
# on every build, so macOS asks again after each one. When a "RoughCut Local
# Signing" code-signing identity is in your keychain (or APPLE_SIGNING_IDENTITY
# names another one), the app is signed with it, so its signature stays the
# same and grants persist. Without one, the build falls back to ad-hoc signing.
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
  # Keep hardened runtime off, as in ad-hoc builds; only notarized releases
  # need it.
  args+=(--config '{"bundle":{"macOS":{"hardenedRuntime":false}}}')
  echo "Signing with \"$identity\"" >&2
else
  echo "No \"$identity\" signing identity found; building ad-hoc (per-app grants reset on every build)." >&2
fi

# ${args[@]+...}: an empty array trips `set -u` in macOS's bash 3.2.
if command -v cargo-tauri >/dev/null; then
  cargo tauri build ${args[@]+"${args[@]}"} "$@"
else
  npx --yes @tauri-apps/cli@^2 build ${args[@]+"${args[@]}"} "$@"
fi
