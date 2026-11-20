#!/usr/bin/env bash
# Print the commit subjects for the PR, push or stable build as inert Markdown text.
set -euo pipefail

head=${GITHUB_SHA:?missing build commit}
base=
case ${BUILD_CHANNEL:-} in
    release)
        # Skip any tag created by an earlier attempt at this same release.
        base=$(git tag --merged "$head" --sort=-version:refname --list 'v[0-9]*' |
            awk -v current="$RELEASE_TAG" '$0 ~ /^v[0-9]+\.[0-9]+\.[0-9]+$/ && $0 != current && !seen++ { print }')
        ;;
    nightly)
        base=$(jq -r '.before // empty' "$GITHUB_EVENT_PATH")
        ;;
    '')
        head=$(jq -er '.pull_request.head.sha' "$GITHUB_EVENT_PATH")
        base=$(jq -er '.pull_request.base.sha' "$GITHUB_EVENT_PATH")
        ;;
    *)
        echo "Unknown build channel: $BUILD_CHANNEL" >&2
        exit 1
        ;;
esac

printf '### Changes in this build\n\n'
format='    %h %s'
if [[ -n $base ]] && git cat-file -e "$base^{commit}" 2> /dev/null &&
    [[ -n $(git rev-list -1 "$base..$head") ]]; then
    git log --reverse --format="$format" "$base..$head"
elif [[ ${BUILD_CHANNEL:-} == nightly ]] && jq -e '.commits | length > 0' "$GITHUB_EVENT_PATH" > /dev/null; then
    # New branches and force pushes can lack a reachable previous revision.
    jq -r '.commits[] | "    \(.id[:12]) \(.message | split("\n")[0])"' "$GITHUB_EVENT_PATH"
else
    git log -1 --format="$format" "$head"
fi
