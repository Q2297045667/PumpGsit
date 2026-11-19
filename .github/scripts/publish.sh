#!/usr/bin/env bash
# Publish a verified bundle. An interrupted draft can be resumed on a rerun.
set -euo pipefail

cd release-assets
sha256sum -c SHA256SUMS
notes="$RUNNER_TEMP/pumpgsit-notes.md"
flags=()
channel=()
tag=${RELEASE_TAG:?missing tag from build}
if [[ "$GITHUB_REF_TYPE" == tag ]]; then
    [[ "$tag" == "$GITHUB_REF_NAME" ]]
    title="pumpgsit $tag"
    flags+=(--verify-tag)
else
    title="pumpgsit ${PLUGIN_VERSION:?missing nightly version from build}"
    channel+=(--prerelease --latest=false)
fi
printf 'Built from [%s](%s/%s/commit/%s).\n\n[Installation and SQLite migration](%s/%s/blob/%s/README.md).\n' \
    "$GITHUB_SHA" "$GITHUB_SERVER_URL" "$GITHUB_REPOSITORY" "$GITHUB_SHA" \
    "$GITHUB_SERVER_URL" "$GITHUB_REPOSITORY" "$GITHUB_SHA" > "$notes"

if draft=$(gh release view "$tag" --repo "$GITHUB_REPOSITORY" --json isDraft --jq .isDraft 2>/dev/null); then
    if [[ "$draft" == false ]]; then
        # A published release is immutable. Confirm its source and all asset bytes.
        [[ "$(gh api "repos/$GITHUB_REPOSITORY/commits/$tag" --jq .sha)" == "$GITHUB_SHA" ]]
        previous=$(mktemp -d)
        gh release download "$tag" --repo "$GITHUB_REPOSITORY" --dir "$previous"
        cmp SHA256SUMS "$previous/SHA256SUMS"
        (cd "$previous" && sha256sum -c SHA256SUMS)
        exit 0
    fi
else
    gh release create "$tag" --repo "$GITHUB_REPOSITORY" --draft --target "$GITHUB_SHA" \
        --title "$title" --notes-file "$notes" "${flags[@]}" "${channel[@]}"
fi

gh release upload "$tag" --repo "$GITHUB_REPOSITORY" --clobber \
    pumpgsit.wasm pumpkin_pie_plugin.wasm pumpkinpie-0.2.6-source.tar.gz SHA256SUMS
gh release edit "$tag" --repo "$GITHUB_REPOSITORY" --draft=false "${channel[@]}"
