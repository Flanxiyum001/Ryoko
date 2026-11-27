#!/usr/bin/env bash
# Source from the preview publication steps. Explicit tag builds are exempt.
# The Releases API has no atomic branch-head precondition. Check on both sides
# of each public mutation and compensate on observed drift or mutation failure.
require_current_preview_head() {
  if [[ "$GITHUB_REF" != refs/heads/ryoko ]]; then
    return 0
  fi
  local head_sha
  if ! head_sha="$(gh api "repos/$GH_REPO/branches/ryoko" --jq '.commit.sha')"; then
    echo "::error::Could not verify the default-branch head." >&2
    return 1
  fi
  if [[ "$head_sha" != "$GITHUB_SHA" ]]; then
    echo "::error::Default-branch head advanced; refusing superseded publication." >&2
    return 1
  fi
}

guarded_preview_publish() {
  local publish="$1" compensate="$2"
  require_current_preview_head || return 1
  if "$publish" && require_current_preview_head; then
    return 0
  fi
  echo "::error::Publication failed or branch head changed during publication; attempting compensation." >&2
  if ! "$compensate"; then
    echo "::error::Publication compensation failed; maintainer intervention required." >&2
  fi
  return 1
}
