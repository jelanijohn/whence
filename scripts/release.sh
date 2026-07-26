#!/usr/bin/env bash
# Cut a Whence release: tag main and push the tag. The Release workflow does
# the rest (see RELEASE.md) — the tag is the single source of truth for the
# version, so nothing in-repo is bumped here.
set -euo pipefail

latest_tag() {
  git tag --sort=-creatordate | head -1
}

die() {
  echo "error: $*" >&2
  exit 1
}

if [[ $# -ne 1 ]]; then
  echo "usage: $0 <version>   e.g. $0 0.2.1  (or v0.2.1)" >&2
  echo "latest tag: $(latest_tag)" >&2
  exit 1
fi

TAG="v${1#v}"
[[ "$TAG" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] \
  || die "'$1' is not a version — expected X.Y.Z (latest tag: $(latest_tag))"

BRANCH=$(git rev-parse --abbrev-ref HEAD)
[[ "$BRANCH" == "main" ]] || die "on branch '$BRANCH' — releases are cut from main"

# -uno: untracked files (scratch docs etc.) don't block a release.
[[ -z $(git status --porcelain -uno) ]] \
  || die "tracked working tree is dirty — commit or stash first"

echo "Syncing with origin/main..."
git pull --ff-only

git rev-parse -q --verify "refs/tags/$TAG" >/dev/null \
  && die "tag $TAG already exists locally"
git ls-remote --exit-code origin "refs/tags/$TAG" >/dev/null 2>&1 \
  && die "tag $TAG already exists on origin"

LATEST=$(latest_tag)
if [[ -n "$LATEST" && "$(printf '%s\n' "$LATEST" "$TAG" | sort -V | tail -1)" != "$TAG" ]]; then
  echo "warning: $TAG does not sort above the latest tag ($LATEST)" >&2
fi

echo
echo "  $LATEST → $TAG  (HEAD: $(git log -1 --format='%h %s'))"
echo
read -r -p "Tag and push $TAG? [y/N] " REPLY
[[ "$REPLY" == [yY]* ]] || { echo "aborted — nothing done"; exit 1; }

git tag "$TAG"
git push origin "$TAG"

# owner/repo from the origin URL, for both git@github.com:o/r.git and https forms.
REPO=$(git remote get-url origin | sed -E 's#^(git@github\.com:|https://github\.com/)##; s#\.git$##')

echo
echo "Pushed $TAG. The Release workflow is building installers:"
echo "  https://github.com/$REPO/actions"
echo "When it finishes, sanity-check and publish the draft release:"
echo "  https://github.com/$REPO/releases"
echo
echo "To re-run a broken release, delete the draft release, then:"
echo "  git push origin :refs/tags/$TAG && git tag -d $TAG"
