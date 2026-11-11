#!/usr/bin/env bash
# Which platform jobs a run of release-beta.yml must build (its "changes" job).
#
# On a pull request a job is skipped when it already passed on this PR at some
# commit S, and nothing it builds from has changed since S. Pushes to dev, tags
# and manual runs build everything: each release stamps a new version into all
# three packages.
#
# Env: EVENT (github.event_name), TO_SHA (the commit this run builds),
# REPO, BRANCH (the PR's head branch), RUN_ID (this run, left out of the
# search), GH_TOKEN. Writes macos=, windows=, android= to $GITHUB_OUTPUT and a
# line per job to $GITHUB_STEP_SUMMARY.
set -euo pipefail

out=${GITHUB_OUTPUT:-/dev/stdout}
summary=${GITHUB_STEP_SUMMARY:-/dev/stderr}
jobs="macos windows android"

# What each job builds from (tools/package-macos.sh, build-android.sh, the
# include_str!s). Shared: every package compiles proto and carries the page.
shared='^(Cargo\.(toml|lock)$|crates/proto/|crates/app/assets/|\.github/workflows/release-beta\.yml$|tools/ci/)'
own() {
  case $1 in
    macos) echo '^(crates/host/|tools/package-macos\.sh$|tools/macos/)' ;;
    # cargo build of the app: not android/ (Kotlin, launcher icons) or Dioxus.toml.
    windows) echo '^crates/app/(src/|build\.rs$|Cargo\.toml$)' ;;
    android) echo '^(crates/app/|crates/host/|tools/build-android\.sh$)' ;;
  esac
}

if [ "${EVENT:?}" != pull_request ]; then
  for j in $jobs; do echo "$j=true" >> "$out"; done
  echo "Builds every package (${EVENT})." >> "$summary"
  exit 0
fi

# The newest earlier run on this PR in which each job passed: its commit and
# run id. Only real builds count, never a skip, so reuse does not chain.
runs=$(gh api -X GET "repos/${REPO:?}/actions/workflows/release-beta.yml/runs" \
  -f branch="${BRANCH:?}" -f event=pull_request -f per_page=30 \
  --jq '.workflow_runs[] | "\(.id) \(.head_sha)"') || runs="" # API trouble: build everything
found=""
while read -r id sha; do
  [ -n "$id" ] && [ "$id" != "${RUN_ID:-}" ] || continue
  passed=$(gh api "repos/$REPO/actions/runs/$id/jobs" --jq '.jobs[] | select(.conclusion == "success") | .name') || passed=""
  for j in $jobs; do
    case " $found " in *" $j="*) continue ;; esac
    if grep -qx "$j" <<< "$passed"; then found="$found $j=$id:$sha"; fi
  done
  [ "$(wc -w <<< "$found")" -lt 3 ] || break
done <<< "$runs"

for j in $jobs; do
  hit=$(tr ' ' '\n' <<< "$found" | sed -n "s/^$j=//p")
  run=${hit%%:*} sha=${hit#*:}
  build=true why="no earlier pass on this PR"
  if [ -n "$hit" ]; then
    if ! git cat-file -e "$sha^{commit}" 2>/dev/null; then
      why="its last pass (${sha:0:7}) is gone from history (force push)"
    else
      # Against the merge commit this run builds: also counts what dev gained
      # since, so a skip is never wrong (it may rebuild more than needed).
      changed=$(git diff --name-only "$sha" "${TO_SHA:?}" | grep -E "$shared|$(own "$j")" || true)
      if [ -z "$changed" ]; then
        build=false why="reused: passed at ${sha:0:7} in [run $run](https://github.com/$REPO/actions/runs/$run), nothing it builds from changed since"
      else
        why="changed since its pass at ${sha:0:7}: $(head -3 <<< "$changed" | tr '\n' ' ')"
      fi
    fi
  fi
  echo "$j=$build" >> "$out"
  echo "- **$j**: $([ "$build" = true ] && echo built || echo skipped). $why" >> "$summary"
done
