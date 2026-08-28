#!/usr/bin/env python3
"""Fetch the captured modules this oracle runs against into ./wasm.

The modules are WhatsApp's artifacts and are not vendored here. `wasm.lock.json`
names the six this repository needs, each with its SHA-256 and the CDN url it
was captured from, followed by the releases that carry `wasm-*.tar.xz` archives
of the same bytes.

The origin url is tried first. WhatsApp's CDN still serves these exact bytes
long after the rollout that produced them, it needs no token, and it costs one
request per module rather than a whole archive. The releases are the fallback
for the day a capture rolls off it.

Pinning by hash rather than by whatever WhatsApp serves today is deliberate.
Every function index, table slot and absolute address recorded in README.md and
VOIP_STATUS.md was read out of these exact bytes. WhatsApp renames the files on
each rollout, and the module behind a name stays the same program — but not the
same binary. A payload whose hash does not match is refused rather than used,
because an oracle that quietly answers from a different build is worse than one
that does not run.

Usage:

    python3 scripts/fetch-wasm.py [dest-dir]

Needs only the standard library and network access to api.github.com. Private
sources need a token: GITHUB_TOKEN or GH_TOKEN if set, otherwise `gh auth
token` is asked for one.
"""

import hashlib
import io
import json
import lzma
import os
import subprocess
import sys
import tarfile
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
LOCK = ROOT / "wasm.lock.json"


def sha256_of(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


class DropAuthOnRedirect(urllib.request.HTTPRedirectHandler):
    """Strips the token when GitHub redirects an asset download to storage.

    Release assets are served by a redirect to a signed URL that carries its own
    credentials. Sending ours along makes the storage backend reject the request
    outright, so a private asset would fail in a way that reads like a bad token.
    """

    def redirect_request(self, req, fp, code, msg, headers, newurl):
        redirected = super().redirect_request(req, fp, code, msg, headers, newurl)
        if redirected is not None:
            redirected.headers.pop("Authorization", None)
            redirected.unredirected_hdrs.pop("Authorization", None)
        return redirected


OPENER = urllib.request.build_opener(DropAuthOnRedirect)


def token() -> str | None:
    """The token to authenticate with, if one can be had without prompting."""
    for name in ("GITHUB_TOKEN", "GH_TOKEN"):
        if os.environ.get(name):
            return os.environ[name]
    try:
        found = subprocess.run(
            ["gh", "auth", "token"],
            capture_output=True,
            text=True,
            timeout=15,
            check=False,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return found.stdout.strip() or None


def get(url: str, accept: str, authenticate: bool = False) -> bytes:
    """Fetches a url, sending our token only where it belongs.

    `authenticate` is off by default and set only for GitHub: the origin CDN is
    a third party, and a token offered to one is a token disclosed to it.
    """
    request = urllib.request.Request(url)
    request.add_header("Accept", accept)
    if authenticate and TOKEN:
        request.add_header("Authorization", f"Bearer {TOKEN}")
    with OPENER.open(request) as response:
        return response.read()


TOKEN = token()


def satisfied(dest: Path, module: dict) -> bool:
    """Whether the module is on disk with the bytes the lock pins.

    Checked by hash, not by presence: a truncated download leaves a file of the
    right name, and every offset the notes record would then be read out of the
    wrong place.
    """
    path = dest / module["fileName"]
    if not path.is_file() or path.stat().st_size != module["size"]:
        return False
    return sha256_of(path) == module["sha256"]


def take_from(archive: bytes, missing: dict, dest: Path, label: str) -> None:
    """Writes out whichever wanted modules this archive carries, by hash."""
    with tarfile.open(fileobj=io.BytesIO(lzma.decompress(archive))) as tar:
        for member in tar.getmembers():
            # Names carry a ./ prefix, and WhatsApp serves ids starting with
            # `-`. Take the basename only: a member is written under the name
            # the lock knows, never under a path the archive chose.
            name = os.path.basename(member.name)
            module = missing.get(name)
            if module is None or not member.isfile():
                continue
            extracted = tar.extractfile(member)
            if extracted is None:
                continue
            payload = extracted.read()
            digest = hashlib.sha256(payload).hexdigest()
            if digest != module["sha256"]:
                print(
                    f"    {name} in {label} is a different capture "
                    f"(sha256 {digest[:12]}…, lock says {module['sha256'][:12]}…)",
                    file=sys.stderr,
                )
                continue
            (dest / name).write_bytes(payload)
            print(f"    {name}  {len(payload)} bytes  ok")
            del missing[name]


def take_from_origin(missing: dict, dest: Path) -> None:
    """Downloads whichever wanted modules the CDN still serves, by hash.

    A mismatch here means the url now answers with a later capture, which is a
    different binary and not an update — it is reported and left on the ground
    for the release archives below to satisfy.
    """
    for name, module in list(missing.items()):
        url = module.get("url")
        if not url:
            continue
        try:
            payload = get(url, "application/octet-stream")
        except (urllib.error.HTTPError, urllib.error.URLError, OSError) as error:
            print(f"    {name}: {error}", file=sys.stderr)
            continue
        digest = hashlib.sha256(payload).hexdigest()
        if digest != module["sha256"]:
            print(
                f"    {name} at the origin is a different capture "
                f"(sha256 {digest[:12]}…, lock says {module['sha256'][:12]}…)",
                file=sys.stderr,
            )
            continue
        (dest / name).write_bytes(payload)
        print(f"    {name}  {len(payload)} bytes  ok")
        del missing[name]


def main() -> int:
    lock = json.loads(LOCK.read_text())
    modules = lock["modules"]

    dest = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "wasm"
    dest.mkdir(parents=True, exist_ok=True)

    wanted = {module["fileName"]: module for module in modules}
    missing = {name: m for name, m in wanted.items() if not satisfied(dest, m)}
    if not missing:
        print(f"all {len(wanted)} modules present in {dest}")
        return 0

    print(f"need {len(missing)} of {len(wanted)}: {', '.join(sorted(missing))}")

    if any(module.get("url") for module in missing.values()):
        print("  static.whatsapp.net — the capture's own origin")
        take_from_origin(missing, dest)

    for source in lock["sources"]:
        if not missing:
            break
        repo, tag = source["repo"], source["release"]
        print(f"  {repo} @ {tag}")
        url = f"https://api.github.com/repos/{repo}/releases/tags/{tag}"
        try:
            release = json.loads(
                get(url, "application/vnd.github+json", authenticate=True)
            )
        except urllib.error.HTTPError as error:
            reason = "not found or not readable with this token" if error.code in (
                403,
                404,
            ) else str(error)
            print(f"    unavailable: {reason}", file=sys.stderr)
            continue

        assets = [
            asset
            for asset in release.get("assets", [])
            if asset["name"].startswith("wasm-") and asset["name"].endswith(".tar.xz")
        ]
        # Newest first: the set a clone needs is far more often the current one,
        # and stopping once everything is satisfied avoids pulling the rest.
        assets.sort(key=lambda asset: asset.get("created_at", ""), reverse=True)

        for asset in assets:
            if not missing:
                break
            print(f"    reading {asset['name']} ({asset['size'] // 1024} KiB)")
            try:
                # The API url rather than browser_download_url: this one honours
                # the token, which a private release needs.
                archive = get(
                    asset["url"], "application/octet-stream", authenticate=True
                )
            except urllib.error.HTTPError as error:
                print(f"    skipped {asset['name']}: {error}", file=sys.stderr)
                continue
            take_from(archive, missing, dest, asset["name"])

    if missing:
        print(
            "\nnot found in any published archive: " + ", ".join(sorted(missing)),
            file=sys.stderr,
        )
        print(
            "Point WA_WASM_DIR at a directory holding them, or publish them as a\n"
            "wasm-*.tar.xz asset on one of the releases in wasm.lock.json. Without\n"
            "them the tests that need those modules skip, and a skipped run proves\n"
            "nothing. If you mean to move to a newer capture instead, read\n"
            "'Following a capture forward' in README.md first — the recorded\n"
            "offsets do not carry over.",
            file=sys.stderr,
        )
        if TOKEN is None:
            print(
                "\nNo token was found, so private sources were skipped. Set\n"
                "GITHUB_TOKEN, or run `gh auth login`.",
                file=sys.stderr,
            )
        return 1

    print(f"\nall {len(wanted)} modules present in {dest}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
