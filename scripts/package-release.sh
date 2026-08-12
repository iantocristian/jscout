#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

target="${1:-}"
version="$(awk -F '"' '/^version =/ { print $2; exit }' Cargo.toml)"
if [[ -z "$version" ]]; then
  echo "could not read jscout version from Cargo.toml" >&2
  exit 1
fi

host="$(rustc -vV | awk '/^host:/ { print $2 }')"
effective_target="${target:-$host}"
binary_name="jscout"
if [[ "$effective_target" == *windows* ]]; then
  binary_name="jscout.exe"
fi

if [[ -n "$target" ]]; then
  cargo build --locked --release --target "$target"
  binary="$repo_root/target/$target/release/$binary_name"
  platform="$target"
else
  cargo build --locked --release
  binary="$repo_root/target/release/$binary_name"
  platform="$host"
fi

if [[ ! -f "$binary" ]]; then
  echo "release binary was not produced at $binary" >&2
  exit 1
fi
if ! command -v npm >/dev/null 2>&1; then
  echo "npm is required to assemble the bundled pi-ai gateway" >&2
  exit 1
fi

output_dir="$repo_root/target/release-packages"
mkdir -p "$output_dir"
bundle="jscout-$version-$platform"
archive="$output_dir/$bundle.tar.gz"
if [[ -e "$archive" ]]; then
  echo "release archive already exists: $archive" >&2
  exit 1
fi

staging="$(mktemp -d "$output_dir/.jscout-package.XXXXXX")"
trap 'rm -rf "$staging"' EXIT
mkdir -p "$staging/$bundle/gateway"
cp "$binary" "$staging/$bundle/$binary_name"
cp README.md "$staging/$bundle/README.md"
cp .env.example "$staging/$bundle/.env.example"
cp gateway/package.json gateway/package-lock.json "$staging/$bundle/gateway/"
cp -R gateway/src "$staging/$bundle/gateway/src"

# Install the exact lockfile into the release tree. The installed binary then
# discovers gateway/src/main.mjs beside itself without a source checkout.
npm ci --omit=dev --ignore-scripts --prefix "$staging/$bundle/gateway" >&2
partial="$staging/$bundle.tar.gz"
tar -C "$staging" -czf "$partial" "$bundle"
mv "$partial" "$archive"
printf '%s\n' "$archive"
