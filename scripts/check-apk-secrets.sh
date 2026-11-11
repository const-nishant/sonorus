#!/usr/bin/env bash
# Fails (exit 1) if any non-empty SONORUS_S3_* value from the environment appears anywhere inside
# an APK or AAB, dex files included. Values are never printed: only the variable name and the
# path of the file that contains it.
#
# Usage: scripts/check-apk-secrets.sh <apk-or-aab>
set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <apk-or-aab>" >&2
    exit 2
fi
artifact="$1"
if [ ! -f "$artifact" ]; then
    echo "not a file: $artifact" >&2
    exit 2
fi

# Shorter values would match by accident and fail the build for no reason.
min_length=6
names=(SONORUS_S3_ACCESS_KEY_ID SONORUS_S3_SECRET_ACCESS_KEY SONORUS_S3_BUCKET SONORUS_S3_ENDPOINT)

checked=()
for name in "${names[@]}"; do
    value="${!name:-}"
    if [ -z "$value" ]; then
        continue
    fi
    if [ "${#value}" -lt "$min_length" ]; then
        echo "skipping $name: shorter than $min_length characters"
        continue
    fi
    checked+=("$name")
done

if [ "${#checked[@]}" -eq 0 ]; then
    echo "no secrets configured to check"
    exit 0
fi

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT
contents="$workdir/contents"
mkdir "$contents"
unzip -q "$artifact" -d "$contents"

leaked=0
for name in "${checked[@]}"; do
    value="${!name}"
    # -e keeps the value out of option parsing; grep reads it from the variable, not from a file.
    if matches="$(grep -a -F -r -l -e "$value" "$contents" || true)" && [ -n "$matches" ]; then
        while IFS= read -r match; do
            echo "LEAK: $name found in ${match#"$contents"/}"
        done <<< "$matches"
        leaked=1
    fi
done

if [ "$leaked" -ne 0 ]; then
    echo "secret check FAILED for $artifact" >&2
    exit 1
fi
echo "secret check passed: ${#checked[@]} value(s) not found in $artifact"
