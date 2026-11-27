#!/usr/bin/env bash
set -euo pipefail
report="$RUNNER_TEMP/ryoko-smoke-report.txt"
printf 'source_sha=%s\nsource_run=%s\nplatform=macos-arm64\n' "$SOURCE_SHA" "$SOURCE_RUN" > "$report"
[[ "$(uname -m)" == arm64 ]]
files=()
while IFS= read -r -d '' file; do files+=("$file"); done < <(find "$RUNNER_TEMP/ryoko-smoke-input" -type f -name '*.dmg' -print0)
[[ "${#files[@]}" -eq 1 ]]
installer="${files[0]}"
printf 'installer_sha256=%s\n' "$(shasum -a 256 "$installer" | cut -d' ' -f1)" >> "$report"
sandbox="$(mktemp -d "$RUNNER_TEMP/ryoko-smoke.XXXXXX")"
mount="$sandbox/mount"
app="$sandbox/Ryoko.app"
mkdir "$mount"
mounted=false
cleanup() {
  local status=$?
  trap - EXIT
  if [[ "$mounted" == true ]]; then hdiutil detach "$mount" >/dev/null || status=1; fi
  if [[ "$status" == 0 || "$mounted" == false ]]; then
    rm -rf "$app"
    printf 'uninstall=removed-disposable-copy\n' >> "$report"
  fi
  exit "$status"
}
trap cleanup EXIT
hdiutil attach "$installer" -readonly -nobrowse -mountpoint "$mount" >/dev/null
mounted=true
test -d "$mount/Ryoko.app"
ditto "$mount/Ryoko.app" "$app"
test -x "$app/Contents/MacOS/ryoko-desktop"
printf 'install=mounted-and-copied\n' >> "$report"
if ! spctl --assess --type execute "$app" >/dev/null 2>&1; then
  printf 'gatekeeper=blocked\nlaunch=manual-required (unsigned preview; no security bypass)\n' >> "$report"
else
  printf 'gatekeeper=accepted\nlaunch=manual-required (native UI session)\n' >> "$report"
fi
hdiutil detach "$mount" >/dev/null
mounted=false
printf 'workspace_edit_save_relaunch=manual-required\nsecret_scan=not-run\n' >> "$report"
