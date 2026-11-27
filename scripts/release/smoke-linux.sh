#!/usr/bin/env bash
set -euo pipefail
report="$RUNNER_TEMP/ryoko-smoke-report.txt"
printf 'source_sha=%s\nsource_run=%s\nplatform=linux-x64\n' "$SOURCE_SHA" "$SOURCE_RUN" > "$report"
mapfile -t files < <(find "$RUNNER_TEMP/ryoko-smoke-input" -type f -name '*.deb')
[[ "${#files[@]}" -eq 1 ]]
installer="${files[0]}"
printf 'installer_sha256=%s\n' "$(sha256sum "$installer" | cut -d' ' -f1)" >> "$report"
package="$(dpkg-deb -f "$installer" Package)"
[[ "$package" =~ ^[a-z0-9][a-z0-9+.-]+$ ]]
if dpkg-query -W -f='${Status}' "$package" 2>/dev/null | grep -q 'install ok installed'; then
  echo 'Refusing to modify a pre-existing package' >&2
  exit 1
fi
cleanup() {
  local status=$?
  trap - EXIT
  if dpkg-query -W -f='${Status}' "$package" 2>/dev/null | grep -q 'install ok installed'; then
    if sudo dpkg --purge "$package" >/dev/null; then
      printf 'uninstall=passed\n' >> "$report"
    else
      printf 'uninstall=failed\n' >> "$report"
      status=1
    fi
  fi
  exit "$status"
}
trap cleanup EXIT
sudo apt-get install -y "$installer"
printf 'install=passed\n' >> "$report"
dpkg -L "$package" | grep -Fx '/usr/bin/ryoko-desktop'
test -x /usr/bin/ryoko-desktop
set +e
xvfb-run -a dbus-run-session -- timeout --kill-after=5s 15s /usr/bin/ryoko-desktop >/dev/null 2>&1
status=$?
set -e
if [[ "$status" != 124 ]]; then
  printf 'launch=failed (exit %s)\n' "$status" >> "$report"
  exit 1
fi
printf 'launch=process-survived-15s (not UI verification)\n' >> "$report"
printf 'workspace_edit_save_relaunch=manual-required\nappimage=manual-required\nsecret_scan=not-run\n' >> "$report"
