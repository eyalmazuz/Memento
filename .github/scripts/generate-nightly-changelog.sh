#!/usr/bin/env bash
set -euo pipefail

# Generate a clean Markdown changelog for nightly builds covering all commits
# since the most recent stable semver tag (e.g. v2.0.2 or v3.0.0).
#
# Output is used both for:
#   1. SUBOMI_CHANGELOG_FILE (baked into the in-app "What's New" modal)
#   2. The GitHub rolling `nightly` prerelease notes

OUT_FILE="${1:-nightly-changelog.md}"
DATE_UTC="$(date -u +%Y-%m-%d)"
SHORT_SHA="$(git rev-parse --short HEAD)"
LAST_TAG="$(git describe --tags --abbrev=0 --match 'v[0-9]*' 2>/dev/null || true)"

if [[ -n "$LAST_TAG" ]]; then
  RANGE="${LAST_TAG}..HEAD"
  SCOPE_LABEL="Changes since ${LAST_TAG}"
else
  RANGE="HEAD~30..HEAD"
  SCOPE_LABEL="Recent changes"
fi

MAX_COMMITS="${MAX_COMMITS:-40}"
mapfile -t COMMITS < <(git log "$RANGE" --no-merges --pretty=format:'%s (%h)' 2>/dev/null || true)

feats=()
perfs=()
fixes=()
others=()
kept=0
omitted=0

re_release='^chore(\(main\))?: release'
re_feat='^feat(\([^)]*\))?!?: '
re_perf='^perf(\([^)]*\))?!?: '
re_fix='^fix(\([^)]*\))?!?: '

for line in "${COMMITS[@]}"; do
  [[ -z "$line" ]] && continue
  # Skip pure release-please / version-bump commits
  if [[ "$line" =~ $re_release ]]; then
    continue
  fi
  if [[ "$kept" -ge "$MAX_COMMITS" ]]; then
    omitted=$((omitted + 1))
    continue
  fi
  kept=$((kept + 1))
  if [[ "$line" =~ $re_feat ]]; then
    feats+=("$line")
  elif [[ "$line" =~ $re_perf ]]; then
    perfs+=("$line")
  elif [[ "$line" =~ $re_fix ]]; then
    fixes+=("$line")
  else
    others+=("$line")
  fi
done

{
  echo "# Nightly Changelog"
  echo ""
  echo "## Nightly (${DATE_UTC} · ${SHORT_SHA})"
  echo ""

  if [[ ${#feats[@]} -eq 0 && ${#perfs[@]} -eq 0 && ${#fixes[@]} -eq 0 && ${#others[@]} -eq 0 ]]; then
    echo "### ${SCOPE_LABEL}"
    echo ""
    echo "* No code changes since ${LAST_TAG:-the previous build}."
  else
    if [[ ${#feats[@]} -gt 0 ]]; then
      echo "### Features"
      echo ""
      for item in "${feats[@]}"; do
        echo "* ${item}"
      done
      echo ""
    fi
    if [[ ${#perfs[@]} -gt 0 ]]; then
      echo "### Performance Improvements"
      echo ""
      for item in "${perfs[@]}"; do
        echo "* ${item}"
      done
      echo ""
    fi
    if [[ ${#fixes[@]} -gt 0 ]]; then
      echo "### Bug Fixes"
      echo ""
      for item in "${fixes[@]}"; do
        echo "* ${item}"
      done
      echo ""
    fi
    if [[ ${#others[@]} -gt 0 ]]; then
      echo "### Other Changes"
      echo ""
      for item in "${others[@]}"; do
        echo "* ${item}"
      done
      echo ""
    fi
    if [[ "$omitted" -gt 0 ]]; then
      echo "* …and ${omitted} earlier commits since ${LAST_TAG:-previous release}."
      echo ""
    fi
  fi
} > "$OUT_FILE"

echo "Generated ${OUT_FILE} (${SHORT_SHA}, base tag: ${LAST_TAG:-none})"
