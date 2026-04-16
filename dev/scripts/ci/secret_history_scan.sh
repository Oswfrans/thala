#!/usr/bin/env bash
set -euo pipefail

# Scan tracked content and reachable git history for high-confidence secret
# patterns. Known fake fixtures live in detector/provider tests and are excluded
# so new real-looking credentials still fail CI.

pattern='(-----BEGIN (RSA |EC |OPENSSH |DSA |PRIVATE )?PRIVATE KEY-----|AKIA[0-9A-Z]{16}|ASIA[0-9A-Z]{16}|ghp_[A-Za-z0-9_]{20,}|github_pat_[A-Za-z0-9_]+|sk-[A-Za-z0-9]{20,}|sk-or-[A-Za-z0-9-]{20,}|xox[baprs]-[A-Za-z0-9-]{10,}|ntn_[A-Za-z0-9]{20,}|AIza[0-9A-Za-z_-]{35}|https://discord\.com/api/webhooks/[0-9]+/[A-Za-z0-9_-]+)'

pathspec=(
  "."
  ":(exclude)Cargo.lock"
  ":(exclude)target/**"
  ":(exclude)src/security/leak_detector.rs"
  ":(exclude)src/providers/bedrock.rs"
  ":(exclude)src/providers/mod.rs"
)

hits_file="$(mktemp)"
trap 'rm -f "$hits_file"' EXIT

git grep -n -I -E "$pattern" -- "${pathspec[@]}" >"$hits_file" || true

while IFS= read -r rev; do
  git grep -n -I -E "$pattern" "$rev" -- "${pathspec[@]}" >>"$hits_file" || true
done < <(git log --all -G "$pattern" --pretty=format:%H -- "${pathspec[@]}" | sort -u)

if [[ -s "$hits_file" ]]; then
  echo "Secret scan found high-confidence credential patterns:" >&2
  sort -u "$hits_file" >&2
  echo >&2
  echo "If a match is a fake test fixture, move it behind an explicit allowlist exclusion in this script." >&2
  exit 1
fi

echo "Secret scan passed."
