#!/usr/bin/env bash
# Publishes pre-built release/* assets through the Gitea API. Re-runs replace
# only assets with the same name on the release for this exact tag.
set -euo pipefail

readonly tag=${1:?usage: publish-gitea.sh vX.Y.Z}
readonly api_url=${GITHUB_API_URL:?GITHUB_API_URL is required}
readonly repository=${GITHUB_REPOSITORY:?GITHUB_REPOSITORY is required}
readonly token=${GITEA_TOKEN:?GITEA_TOKEN is required}
readonly release_api="${api_url}/repos/${repository}/releases"
readonly auth_header="Authorization: token ${token}"

[[ -d release ]] || { printf 'error: release/ does not exist\n' >&2; exit 1; }

release_json=$(mktemp)
trap 'rm -f -- "$release_json"' EXIT
release_status=$(curl --silent --output "$release_json" --write-out '%{http_code}' \
    --header "$auth_header" "${release_api}/tags/${tag}")
if [[ $release_status == 404 ]]; then
    python3 -c 'import json, sys; print(json.dumps({"tag_name": sys.argv[1], "name": sys.argv[1], "draft": False, "prerelease": "-" in sys.argv[1]}))' \
        "$tag" \
        | curl --fail --silent --show-error --header "$auth_header" \
            --header 'Content-Type: application/json' --data-binary @- \
            "$release_api" > "$release_json"
elif [[ $release_status != 200 ]]; then
    printf 'error: could not get Gitea release for %s (HTTP %s)\n' "$tag" "$release_status" >&2
    exit 1
fi

release_id=$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1]))["id"])' "$release_json")

for asset in release/*; do
    asset_name=$(basename "$asset")
    existing_asset_id=$(python3 -c '
import json
import sys

for asset in json.load(open(sys.argv[1])).get("assets", []):
    if asset["name"] == sys.argv[2]:
        print(asset["id"])
        break
' "$release_json" "$asset_name")
    if [[ -n $existing_asset_id ]]; then
        curl --fail --silent --show-error --request DELETE --header "$auth_header" \
            "${release_api}/${release_id}/assets/${existing_asset_id}" > /dev/null
    fi
    curl --fail --silent --show-error --request POST --header "$auth_header" \
        --header 'Content-Type: application/octet-stream' \
        --data-binary "@${asset}" \
        "${release_api}/${release_id}/assets?name=${asset_name}" > /dev/null
done
