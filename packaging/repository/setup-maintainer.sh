#!/usr/bin/env bash
set -euo pipefail

command -v gpg >/dev/null || { echo 'gpg is required' >&2; exit 1; }
command -v gh >/dev/null || { echo 'GitHub CLI (gh) is required' >&2; exit 1; }
gh auth status >/dev/null

repository="${1:-$(gh repo view --json nameWithOwner --jq .nameWithOwner)}"
[[ "$repository" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || {
  echo 'Usage: setup-maintainer.sh [OWNER/REPOSITORY]' >&2
  exit 2
}

signing_home=''
cleanup() {
  if [[ -n "$signing_home" ]]; then
    GNUPGHOME="$signing_home" gpgconf --kill gpg-agent >/dev/null 2>&1 || true
    rm -rf -- "$signing_home"
  fi
}
trap cleanup EXIT

enabled="$(gh variable get REPO_ENABLED --repo "$repository" 2>/dev/null || true)"
fingerprint="$(gh variable get REPO_SIGNING_FINGERPRINT --repo "$repository" 2>/dev/null || true)"
if [[ "$enabled" != true || ! "$fingerprint" =~ ^[0-9A-F]{40}$|^[0-9A-F]{64}$ ]]; then
  signing_home="$(mktemp -d)"
  chmod 700 "$signing_home"
  export GNUPGHOME="$signing_home"

  identity="Zalo for Linux repository <actions@users.noreply.github.com>"
  gpg --batch --pinentry-mode loopback --passphrase '' \
    --quick-generate-key "$identity" rsa3072 sign 0
  fingerprint="$(gpg --batch --with-colons --list-secret-keys "$identity" |
    awk -F: '$1 == "fpr" { print $10; exit }')"
  [[ "$fingerprint" =~ ^[0-9A-F]{40}$|^[0-9A-F]{64}$ ]] || {
    echo 'Could not determine the signing-key fingerprint' >&2
    exit 1
  }

  gpg --batch --armor --export-secret-keys "$fingerprint" |
    gh secret set REPO_SIGNING_KEY --repo "$repository"
  gh variable set REPO_SIGNING_FINGERPRINT --repo "$repository" --body "$fingerprint"
  gh variable set REPO_ENABLED --repo "$repository" --body true
else
  printf 'Existing signing configuration found; keeping fingerprint %s.\n' "$fingerprint"
fi

pages_endpoint="repos/$repository/pages"
if gh api "$pages_endpoint" >/dev/null 2>&1; then
  gh api --method PUT "$pages_endpoint" -f build_type=workflow >/dev/null
else
  gh api --method POST "$pages_endpoint" -f build_type=workflow >/dev/null
fi

gh workflow run build.yml --repo "$repository" -f force_build=true

printf 'Repository signing is configured for %s.\n' "$repository"
printf 'Public fingerprint: %s\n' "$fingerprint"
printf 'GitHub Pages is configured and the first repository build has been queued.\n'
