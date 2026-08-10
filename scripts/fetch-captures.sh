#!/usr/bin/env bash
# Fetch the wasm captures the `#[ignore]`d test tiers run against, into
# `fixtures/wasm`.
#
#   ./scripts/fetch-captures.sh [<out-dir>]
#
# The captures are WhatsApp Web's own wasm payloads. They are not committed
# here — they are megabytes of somebody else's build output, and they roll —
# so they come from the `bundle-store` release of the public `oxidezap/whatspec`
# repository, which archives them content-addressed.
#
# Every file is checked against `fixtures/captures.sha256` after extraction. A
# capture that arrives with different bytes is not the module the tests pin
# their numbers to, and silently accepting it would turn an exact assertion
# about a real module into an assertion about whatever was downloaded.
#
# WhatsApp rolls these payloads, so an archive is searched for the names the
# manifest asks for rather than assumed to hold them: several archives are
# published and a given capture lives in whichever ones were current while it
# was being served. If a name is in none of them it has aged out of every
# published set, and the tests that need it say so by name.
set -euo pipefail
cd "$(dirname "$0")/.."

out="${1:-fixtures/wasm}"
manifest="fixtures/captures.sha256"
repo="oxidezap/whatspec"
tag="bundle-store"

[ -f "$manifest" ] || { echo "no $manifest — run this from a checkout" >&2; exit 1; }
# Absolute, because the verification below runs with the output directory as its
# working directory and that directory is an argument.
manifest_abs="$PWD/$manifest"

for tool in curl tar sha256sum; do
  command -v "$tool" >/dev/null 2>&1 || { echo "$tool is required and was not found" >&2; exit 1; }
done
# The archives are .tar.xz; GNU tar shells out to xz rather than decoding it.
command -v xz >/dev/null 2>&1 || { echo "xz is required and was not found" >&2; exit 1; }

mkdir -p "$out"

# What the manifest asks for, and what of that is already here and correct. A
# re-run after a partial download must fetch only what is still missing.
wanted=$(awk '{print $2}' "$manifest_abs")
verifies() {
  local name="$1" expected actual
  [ -f "$out/$name" ] || return 1
  expected=$(awk -v n="$name" '$2 == n { print $1 }' "$manifest_abs")
  actual=$(sha256sum -- "$out/$name" | cut -d' ' -f1)
  [ -n "$expected" ] && [ "$expected" = "$actual" ]
}

missing=""
for name in $wanted; do
  verifies "$name" || missing="$missing $name"
done

if [ -z "${missing// /}" ]; then
  echo "all $(echo "$wanted" | wc -w) captures are present and verified in $out"
  exit 0
fi
echo "missing:${missing}"

# The asset names are content-addressed on the payload set, so they change with
# every roll; ask the release what it actually holds rather than pinning a name.
echo "asking $repo for the $tag release..."
urls=$(curl -fsSL "https://api.github.com/repos/$repo/releases/tags/$tag" \
  | grep -o '"browser_download_url": *"[^"]*wasm-[0-9a-f]\{64\}\.tar\.xz"' \
  | sed 's/.*"\(https[^"]*\)"$/\1/')
[ -n "$urls" ] || { echo "no wasm-*.tar.xz assets on $repo@$tag" >&2; exit 1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Every archive, until nothing is missing. The release does not promise an order
# and no single archive holds every capture the manifest names, so this walks
# them rather than guessing which one is enough; each is dropped as soon as it
# has been searched, so the disk cost is one archive rather than all of them.
for url in $(echo "$urls" | tac); do
  [ -n "${missing// /}" ] || break
  archive="$work/$(basename "$url")"
  echo "  fetching $(basename "$url")"
  curl -fsSL -o "$archive" "$url"
  members=$(tar -tf "$archive")
  still_missing=""
  for name in $missing; do
    if echo "$members" | grep -qxF "./$name"; then
      tar -xf "$archive" -C "$out" "./$name"
      echo "    extracted $name"
    else
      still_missing="$still_missing $name"
    fi
  done
  missing="$still_missing"
  rm -f "$archive"
done

if [ -n "${missing// /}" ]; then
  echo >&2
  echo "these captures are in no published archive:${missing}" >&2
  echo "WhatsApp has rolled them and the whatspec release no longer carries them." >&2
  echo "The tests that need them will say so by name rather than passing quietly." >&2
  exit 1
fi

# Verify everything, not only what was just downloaded: a file already present
# from an earlier run is as capable of being wrong as one that just arrived.
(cd "$out" && sha256sum -c "$manifest_abs")
echo "$(echo "$wanted" | wc -w) captures in $out, all verified"
