#!/usr/bin/env bash
# Publish a verified bundle. An interrupted draft can be resumed on a rerun.
set -euo pipefail

cd release-assets
sha256sum PumpGsit.wasm | cmp - SHA256SUMS
notes="$RUNNER_TEMP/PumpGsit-notes.md"
flags=()
channel=()
tag=${RELEASE_TAG:?missing tag from build}

# Check the remote attachment names before publishing a draft or accepting a rerun.
check_assets() {
    local names
    names=$(gh release view "$tag" --repo "$GITHUB_REPOSITORY" --json assets --jq '[.assets[].name] | sort | .[]')
    if [[ "$names" != $'PumpGsit.wasm\nSHA256SUMS' ]]; then
        printf 'Unexpected release attachments for %s:\n%s\n' "$tag" "$names" >&2
        return 1
    fi
}

if [[ "$GITHUB_REF_TYPE" == tag ]]; then
    [[ "$tag" == "$GITHUB_REF_NAME" ]]
    title="PumpGsit $tag"
    flags+=(--verify-tag)
else
    title="PumpGsit ${PLUGIN_VERSION:?missing nightly version from build}"
    channel+=(--prerelease --latest=false)
fi
repo_url="$GITHUB_SERVER_URL/$GITHUB_REPOSITORY"
{
    printf 'Build target wasm32-wasip3 (WASI 0.3), using the Pumpkin Wasm 0.2 plugin API.\n\n'
    printf 'Built from [%s](%s/commit/%s). [Installation](%s/blob/%s/README.md).\n\n' \
        "$GITHUB_SHA" "$repo_url" "$GITHUB_SHA" "$repo_url" "$GITHUB_SHA"
    printf '%s\n\n' 'Upgrading from lowercase pumpgsit? Stop the server, back up plugins/data/pumpgsit, and migrate the data directory, plugin override and permission prefix to PumpGsit before startup.'
    printf '[Upgrade and rollback instructions](%s/blob/%s/doc/存储.md).\n' "$repo_url" "$GITHUB_SHA"
} > "$notes"

if draft=$(gh release view "$tag" --repo "$GITHUB_REPOSITORY" --json isDraft --jq .isDraft 2>/dev/null); then
    if [[ "$draft" == false ]]; then
        # A published release is immutable. Confirm its source and all asset bytes.
        [[ "$(gh api "repos/$GITHUB_REPOSITORY/commits/$tag" --jq .sha)" == "$GITHUB_SHA" ]]
        check_assets
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

gh release upload "$tag" --repo "$GITHUB_REPOSITORY" --clobber PumpGsit.wasm SHA256SUMS
check_assets
gh release edit "$tag" --repo "$GITHUB_REPOSITORY" --draft=false "${channel[@]}"
