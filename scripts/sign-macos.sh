#!/bin/bash
# Sign a staged local build before installing it, using the release identity.
set -euo pipefail

if [[ $(uname -s) != Darwin || $# != 1 ]]; then
    echo "Usage (macOS): CODESIGN_IDENTITY='Developer ID Application: ...' $0 BINARY" >&2
    exit 1
fi
: "${CODESIGN_IDENTITY:?Set CODESIGN_IDENTITY to the release Developer ID Application identity}"

/usr/bin/codesign --force --sign "$CODESIGN_IDENTITY" \
    --identifier com.geekforbrains.enso --options runtime --timestamp "$1"
/usr/bin/codesign --verify --strict \
    --test-requirement='=identifier "com.geekforbrains.enso" and anchor apple generic and certificate leaf[field.1.2.840.113635.100.6.1.13] exists' "$1"
/usr/bin/codesign --display --requirements - "$1"
