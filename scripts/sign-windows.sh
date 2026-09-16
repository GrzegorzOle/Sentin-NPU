#!/usr/bin/env bash
# Copyright 2026 Grzegorz Oleksy
# SPDX-License-Identifier: Apache-2.0
#
# Sign the Windows assets of a published release and put them back.
#
#   scripts/sign-windows.sh <version> [--upload]
#
# Without --upload it signs, verifies and stops, leaving everything in dist/signed-<version>/ to be
# looked at. That order is deliberate: the upload replaces assets people may already have
# downloaded, so it is the one step that should never happen as a side effect of trying something.
#
# Why this is not in the release workflow, and cannot be: the signing key lives on a cryptographic
# card in a reader attached to one machine, and it asks for a PIN. A CI runner has neither. So the
# workflow publishes unsigned assets and this script replaces the Windows ones afterwards, from the
# machine that holds the card.
#
# What it signs, and why not only the installer. SmartScreen judges the file that was downloaded,
# which is the installer alone - but the gateway runs as a service under LocalSystem and the console
# asks for elevation, and both of those show the publisher to the person deciding whether to allow
# them. A chain that stops at the installer leaves "Unknown publisher" on the two dialogs where it
# is read most carefully. So the four binaries are signed first, the installer is then built from
# the signed payload, and the bundle zip gets the same signed binaries - which keeps the property
# the release workflow was built around, that the installer and the zip carry identical binaries.

set -euo pipefail

VERSION="${1:?usage: sign-windows.sh <version> [--upload]}"
UPLOAD="${2:-}"
TAG="v${VERSION}"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# The certificate to sign with, by subject. Overridable because this file is public and somebody
# else's fork will hold a different name; /a then picks the valid one when the card carries several
# generations of the same subject, which it does after the first renewal.
SUBJECT="${SENTIN_SIGN_SUBJECT:-Open Source Developer Grzegorz Robert Oleksy}"
# Certum's timestamp service. Timestamping is not optional: without it every signature made with
# this certificate stops verifying the day the certificate expires, which for an installer people
# keep around is the same as never having signed it.
TIMESTAMP="${SENTIN_SIGN_TIMESTAMP:-http://time.certum.pl}"

say()  { printf '\033[1m==>\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

# Git Bash rewrites any argument that starts with a slash into a Windows path, so /fd would reach
# signtool as F:\d and /DVersion would reach ISCC as a second script name. This cost the packaging
# workflow a build once already.
export MSYS_NO_PATHCONV=1

# The newest signtool present, rather than a pinned one: the SDK version that happens to be
# installed is not a property of this project, and any of them signs the same bytes.
SIGNTOOL="$(ls -d "/c/Program Files (x86)/Windows Kits/10/bin/"*/x64/signtool.exe 2>/dev/null | sort -V | tail -1)"
[ -n "${SIGNTOOL}" ] || die "no signtool - install the Windows SDK signing tools"

# chocolatey (what CI uses) puts Inno Setup in Program Files, winget puts it under the user's
# AppData. Look in both rather than making the reader guess which installer they ran.
ISCC=""
for candidate in \
    "/c/Program Files (x86)/Inno Setup 6/ISCC.exe" \
    "/c/Program Files/Inno Setup 6/ISCC.exe" \
    "${LOCALAPPDATA:-/c/Users/${USERNAME:-}/AppData/Local}/Programs/Inno Setup 6/ISCC.exe"; do
    [ -f "${candidate}" ] && { ISCC="${candidate}"; break; }
done
[ -n "${ISCC}" ] || die "no ISCC - install Inno Setup 6 (winget install JRSoftware.InnoSetup)"

WORK="${HERE}/dist/signed-${VERSION}"
BUNDLE="sentin-npu-diag-${VERSION}-windows-x64"
SETUP="sentin-npu-setup-${VERSION}.exe"

rm -rf "${WORK}"
mkdir -p "${WORK}"
cd "${WORK}"

say "fetching the unsigned assets of ${TAG}"
gh release download "${TAG}" --pattern "${BUNDLE}.zip" --pattern "SHA256SUMS.txt" --dir .
cp "${BUNDLE}.zip" "${BUNDLE}-unsigned.zip"
unzip -q "${BUNDLE}.zip"

say "signing the binaries"
# All four in one invocation, because the card asks for the PIN per invocation and four prompts
# invite the one that gets cancelled halfway.
"${SIGNTOOL}" sign /n "${SUBJECT}" /a /fd sha256 /tr "${TIMESTAMP}" /td sha256 \
    /d "Sentin-NPU" /du "https://github.com/GrzegorzOle/Sentin-NPU" \
    "${BUNDLE}/sentin-gateway.exe" \
    "${BUNDLE}/sentin-ui.exe" \
    "${BUNDLE}/sentin-doctor.exe" \
    "${BUNDLE}/sentin-bench.exe"

say "rebuilding the bundle around the signed binaries"
# Rewritten from the original archive rather than zipped up from the directory: every entry keeps
# the metadata and the order the release workflow gave it, and the only difference between the two
# archives is the four files that were signed. A fresh `zip` of the same tree would differ in ways
# nobody could tell apart from tampering.
python - "${BUNDLE}-unsigned.zip" "${BUNDLE}.zip" "${BUNDLE}" <<'PY'
import pathlib, shutil, sys, zipfile

source, target, prefix = sys.argv[1], sys.argv[2], sys.argv[3]
signed = {
    f"{prefix}/{name}"
    for name in ("sentin-gateway.exe", "sentin-ui.exe", "sentin-doctor.exe", "sentin-bench.exe")
}
replaced = set()

with zipfile.ZipFile(source) as old, zipfile.ZipFile(target, "w", zipfile.ZIP_DEFLATED) as new:
    for item in old.infolist():
        if item.filename in signed:
            data = pathlib.Path(item.filename).read_bytes()
            replaced.add(item.filename)
        else:
            data = old.read(item)
        # The ZipInfo travels with the entry, so timestamps, attributes and the compression choice
        # survive; only the bytes of the four signed files change.
        new.writestr(item, data)

missing = signed - replaced
if missing:
    raise SystemExit(f"not in the archive, so not replaced: {sorted(missing)}")
print(f"  replaced {len(replaced)} entries")
PY

say "building the installer from the signed payload"
"${ISCC}" "/DVersion=${VERSION}" "/DPayload=$(cd "${BUNDLE}" && pwd -W)" \
    "$(cd "${HERE}/packaging/windows" && pwd -W)/sentin-npu.iss" >/dev/null
cp "${HERE}/packaging/windows/out/${SETUP}" .

say "signing the installer"
"${SIGNTOOL}" sign /n "${SUBJECT}" /a /fd sha256 /tr "${TIMESTAMP}" /td sha256 \
    /d "Sentin-NPU installer" /du "https://github.com/GrzegorzOle/Sentin-NPU" "${SETUP}"

say "verifying every signature"
# Verified rather than assumed: signtool reports success on signing a file it could not timestamp
# in some configurations, and an untimestamped signature is the failure that only shows up in 2027.
for file in "${SETUP}" "${BUNDLE}/sentin-gateway.exe" "${BUNDLE}/sentin-ui.exe" \
            "${BUNDLE}/sentin-doctor.exe" "${BUNDLE}/sentin-bench.exe"; do
    "${SIGNTOOL}" verify /pa /q "${file}" || die "verification failed for ${file}"
    "${SIGNTOOL}" verify /pa /v "${file}" 2>/dev/null | grep -q "signature is timestamped" \
        || die "no timestamp on ${file}"
    printf '  %s  signed and timestamped\n' "${file}"
done

say "rewriting the two checksum lines that changed"
# Only the Windows zip and the installer move. The Linux bundle, the AppImage, the docs archive and
# the two model tarballs are untouched, and rewriting their lines would say otherwise.
# sha256sum marks binary mode with a leading '*' on Windows; the workflow's lines have no such mark,
# and a checksum file formatted two ways invites somebody to write a parser for it.
for asset in "${BUNDLE}.zip" "${SETUP}"; do
    sum="$(sha256sum "${asset}" | sed 's/ \*/  /' | cut -d' ' -f1)"
    grep -q "  ${asset}\$" SHA256SUMS.txt || die "no line for ${asset} in SHA256SUMS.txt"
    sed -i "s|^[0-9a-f]\{64\}  ${asset}\$|${sum}  ${asset}|" SHA256SUMS.txt
    printf '  %s  %s\n' "${sum}" "${asset}"
done

if [ "${UPLOAD}" != "--upload" ]; then
    say "signed, not uploaded"
    echo "  everything is in ${WORK}"
    echo "  to publish: scripts/sign-windows.sh ${VERSION} --upload"
    exit 0
fi

say "replacing the assets on ${TAG}"
gh release upload "${TAG}" "${BUNDLE}.zip" "${SETUP}" SHA256SUMS.txt --clobber
say "done - the Windows assets of ${TAG} are signed"
