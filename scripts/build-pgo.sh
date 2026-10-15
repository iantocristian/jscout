#!/usr/bin/env bash
set -euo pipefail

# One local training campaign; never changes the ordinary release artifacts.
fail() { echo "build-pgo: $*" >&2; exit 1; }
if [[ $# != 2 || ! "$1" =~ ^(baseline|instrument|optimize)$ ]]; then
  fail "usage: bash scripts/build-pgo.sh {baseline|instrument|optimize} /absolute/artifact-root"
fi
mode="$1"
artifact_root="$2"
[[ "$artifact_root" == /* && "$artifact_root" != / ]] || fail "artifact root must be an absolute directory, not /"
[[ "$artifact_root" != *[[:space:]]* ]] || fail "artifact root must not contain whitespace (RUSTFLAGS paths)"
repo_root="$(cd "$(dirname "$0")/.." && pwd -P)"
mkdir -p "$artifact_root"
artifact_root="$(cd "$artifact_root" && pwd -P)"
case "$artifact_root/" in
  "$repo_root/"*) fail "keep build and profile artifacts outside the source checkout" ;;
esac
cd "$repo_root"

# Do not silently combine a reproducible recipe with ambient compiler overrides.
for variable in $(compgen -e); do
  case "$variable" in
    RUSTFLAGS|CARGO_ENCODED_RUSTFLAGS|CARGO_PROFILE_*|\
    RUSTC|RUSTC_WRAPPER|RUSTC_WORKSPACE_WRAPPER|CARGO_BUILD_RUSTC*|\
    CC|CXX|CFLAGS|CXXFLAGS|CPPFLAGS|AR|ARFLAGS|\
    CC_*|CXX_*|CFLAGS_*|CXXFLAGS_*|CPPFLAGS_*|AR_*|ARFLAGS_*|\
    HOST_CC|HOST_CXX|HOST_CFLAGS|HOST_CXXFLAGS|HOST_AR|HOST_ARFLAGS|\
    TARGET_CC|TARGET_CXX|TARGET_CFLAGS|TARGET_CXXFLAGS|TARGET_AR|TARGET_ARFLAGS|\
    CRATE_CC_NO_DEFAULTS|LIBSQLITE3_FLAGS)
      [[ -z "${!variable}" ]] || fail "unset $variable for this build"
      ;;
  esac
done
target="$(rustc -vV | sed -n 's/^host: //p')"
[[ "$target" == aarch64-apple-darwin ]] || fail "this native build recipe requires Apple Silicon macOS"
profdata="$(rustc --print sysroot)/lib/rustlib/$target/bin/llvm-profdata"
[[ -x "$profdata" ]] || fail "matching llvm-profdata is missing; install this toolchain's llvm-tools-preview component"
common_flags="-C target-cpu=native -C force-frame-pointers=yes"
profile_dir="$artifact_root/profiles"
raw_dir="$profile_dir/raw"
merged="$profile_dir/merged.profdata"
identity_file="$profile_dir/build-inputs.sha256"
campaign_identity_file="$artifact_root/build-inputs.sha256"
stage_identity_file="$artifact_root/build-$mode-inputs.sha256"
build_dir="$artifact_root/build-$mode"
build_log="$artifact_root/build-$mode.log"
binary="$build_dir/$target/release-perf/jscout"
[[ ! -e "$build_dir" ]] || fail "build output already exists: $build_dir; use a fresh artifact root"
[[ ! -e "$build_log" ]] || fail "build log already exists: $build_log; use a fresh artifact root"
[[ ! -e "$stage_identity_file" ]] || fail "stage inputs already exist: $stage_identity_file; use a fresh artifact root"

c_toolchain() {
  command -v cc c++
  cc --version
  c++ --version
  printf 'SDKROOT=%s\nDEVELOPER_DIR=%s\nMACOSX_DEPLOYMENT_TARGET=%s\n' \
    "${SDKROOT-}" "${DEVELOPER_DIR-}" "${MACOSX_DEPLOYMENT_TARGET-}"
}

# Hash build inputs, including embedded guides/config, without unrelated docs.
# Keep compiler, flags and native CPU identical between collection and use.
build_identity() {
  {
    rustc -vV
    cargo --version
    c_toolchain
    rustc --print target-cpus | sed -n '/native /p'
    printf '%s\n' "$common_flags" 'profile=release-perf'
    git ls-files -z --cached --others --exclude-standard -- \
      Cargo.toml Cargo.lock rust-toolchain.toml build.rs .cargo src \
      .jscout.toml.example integrations/jscout |
      xargs -0 shasum -a 256 | LC_ALL=C sort
  } | shasum -a 256 | cut -d ' ' -f 1
}
identity="$(build_identity)"
flags="$common_flags"
case "$mode" in
  baseline) ;;
  instrument)
    mkdir -p "$raw_dir"
    shopt -s nullglob dotglob
    existing=("$raw_dir"/*)
    [[ ${#existing[@]} == 0 && ! -e "$merged" ]] || fail "profiles already exist; start a fresh artifact root"
    flags="$common_flags -C profile-generate=$raw_dir"
    ;;
  optimize)
    [[ -f "$identity_file" ]] || fail "run instrument and train its binary first"
    [[ "$(<"$identity_file")" == "$identity" ]] || fail "build inputs changed since instrumentation; start a fresh artifact root"
    [[ ! -e "$merged" ]] || fail "merged.profdata already exists; use a fresh artifact root rather than overwrite training evidence"
    shopt -s nullglob dotglob
    raw_files=("$raw_dir"/*)
    [[ ${#raw_files[@]} != 0 ]] || fail "no training profiles found"
    for raw in "${raw_files[@]}"; do
      [[ "$raw" == *.profraw && -f "$raw" && -s "$raw" ]] || fail "unexpected or empty training profile: $raw"
    done
    flags="$common_flags -C profile-use=$merged -C llvm-args=-pgo-warn-missing-function"
    ;;
esac

# Publish a complete identity atomically, including when baseline and instrument
# start together. Retain each stage's input identity alongside its build log.
printf '%s\n' "$identity" > "$stage_identity_file"
ln "$stage_identity_file" "$campaign_identity_file" 2>/dev/null ||
  [[ "$(<"$campaign_identity_file")" == "$identity" ]] ||
  fail "build inputs changed within this campaign; start a fresh artifact root"

{
  printf 'mode=%s\ntarget=%s\nprofile=release-perf\nrustflags=%s\nbuild_inputs_sha256=%s\n' \
    "$mode" "$target" "$flags" "$identity"
  c_toolchain
  if [[ "$mode" == optimize ]]; then
    # Preserve zero-count functions: --sparse can inhibit PGO optimization.
    "$profdata" merge --failure-mode=any -o "$merged" "${raw_files[@]}"
    "$profdata" show "$merged"
  fi
  CARGO_INCREMENTAL=0 CARGO_TARGET_DIR="$build_dir" RUSTFLAGS="$flags" \
    cargo build --locked --profile release-perf --target "$target" --bin jscout
  [[ -x "$binary" ]] || fail "Cargo did not produce $binary"
  [[ "$(build_identity)" == "$identity" ]] || fail "build inputs changed during compilation; discard this campaign"
  if [[ "$mode" == instrument ]]; then
    printf '%s\n' "$identity" > "$identity_file"
  fi
  printf 'binary=%s\nraw_profiles=%s\nmerged_profile=%s\nbuild_log=%s\n' \
    "$binary" "$raw_dir" "$merged" "$build_log"
  shasum -a 256 "$binary"
} 2>&1 | tee "$build_log"
