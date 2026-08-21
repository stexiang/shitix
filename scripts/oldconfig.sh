#!/usr/bin/env bash
#
# Non-interactively refresh `.config`: keep existing values, append any new
# options that exist in config/defaults but are missing from `.config`.
# Mirrors the Linux kernel's `make oldconfig` (without the prompting).
#
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

DEFAULTS="config/defaults"
CONFIG_FILE=".config"

if [[ ! -f "$CONFIG_FILE" ]]; then
    cp "$DEFAULTS" "$CONFIG_FILE"
    echo "  created .config from config/defaults"
    exit 0
fi

added=0
while IFS='=' read -r k v; do
    [[ -z "$k" ]] && continue
    k="${k//[[:space:]]/}"
    v="${v//[[:space:]]/}"
    if ! grep -qE "^${k}=" "$CONFIG_FILE"; then
        printf '%s=%s\n' "$k" "$v" >> "$CONFIG_FILE"
        echo "  + ${k}=${v}"
        added=$((added + 1))
    fi
done < <(grep -vE '^\s*(#|$)' "$DEFAULTS")

if [[ $added -eq 0 ]]; then
    echo "  .config is up to date"
else
    echo "  added $added new option(s)"
fi
