#!/usr/bin/env bash
# Prints the release notes for v$VERSION as Markdown: every commit since the
# last published release, the people committing for the first time, and a
# link to the full diff. Commits are read from the GitHub API so authors show
# as @login when their email is linked to a GitHub account.
#
# Needs: VERSION, GH_REPO, GITHUB_SHA, GH_TOKEN, a full clone (fetch-depth: 0).
set -euo pipefail

tag="v$VERSION"
server="${GITHUB_SERVER_URL:-https://github.com}"

# Last published release before this one: the newest v* tag reachable from
# HEAD that isn't this version's own tag. Draft releases have no tag yet.
prev=$(git tag --merged "$GITHUB_SHA" --list 'v[0-9]*' --sort=-v:refname | grep -vx "$tag" | head -n1 || true)

# One line per commit: sha <TAB> author <TAB> subject, oldest first; merges skipped.
commit_jq='select(.parents | length == 1)
  | [.sha, (if .author.login then "@" + .author.login else .commit.author.name end),
     (.commit.message | split("\n")[0])] | @tsv'

commits_since() { # $1 = previous tag or empty for the whole history
  if [ -n "$1" ]; then
    gh api --paginate "repos/$GH_REPO/compare/$1...$GITHUB_SHA" --jq ".commits[] | $commit_jq"
  else
    gh api --paginate "repos/$GH_REPO/commits?sha=$GITHUB_SHA&per_page=100" --jq ".[] | $commit_jq" | tac
  fi
}

authors_before() { # $1 = previous tag; prints everyone who committed up to it
  [ -n "$1" ] || return 0
  gh api --paginate "repos/$GH_REPO/commits?sha=$1&per_page=100" \
    --jq '.[] | if .author.login then "@" + .author.login else .commit.author.name end'
}

changes=$(commits_since "$prev")
known=$(authors_before "$prev" | sort -u)

echo "## What's Changed"
echo
if [ -z "$changes" ]; then
  echo "No changes since $prev."
else
  # Newest first, like a changelog.
  printf '%s\n' "$changes" | tac | while IFS=$'\t' read -r sha author subject; do
    echo "- $subject by $author in $sha"
  done
fi

# Authors in this range who never committed before, in order of first commit.
newcomers=$(printf '%s\n' "$changes" | awk -F'\t' 'NF && !seen[$2]++ { print $2 "\t" $1 }' \
  | while IFS=$'\t' read -r author sha; do
      grep -qxF -- "$author" <<<"$known" || printf '%s\t%s\n' "$author" "$sha"
    done)
if [ -n "$newcomers" ]; then
  echo
  echo "## New Contributors"
  echo
  printf '%s\n' "$newcomers" | while IFS=$'\t' read -r author sha; do
    echo "- $author made their first contribution in $sha"
  done
fi

echo
if [ -n "$prev" ]; then
  echo "**Full Changelog**: $server/$GH_REPO/compare/$prev...$tag"
else
  echo "**Full Changelog**: $server/$GH_REPO/commits/$tag"
fi
