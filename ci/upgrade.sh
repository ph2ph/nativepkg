#!/bin/bash
# Upgrade scenarios: what the old implementation got wrong and no install-only test can see.
# `prerm` ignored dpkg's action argument and deleted `node_modules` on upgrade; `postinst`
# called `systemctl enable` unconditionally and re-enabled a unit the administrator disabled.
set -euo pipefail

old="${1:?usage: upgrade.sh <old.deb> <new.deb> <name>}"
new="${2:?usage: upgrade.sh <old.deb> <new.deb> <name>}"
name="${3:?usage: upgrade.sh <old.deb> <new.deb> <name>}"
fail=0

check() {
  if eval "$2" >/dev/null 2>&1; then printf '  ok    %s\n' "$1"
  else printf '  FAIL  %s\n' "$1"; fail=1; fi
}

printf '\n=== install the old version\n'
dpkg --install "$old"

# Something the administrator did, and an upgrade must not undo.
printf 'ADMIN_SETTING=kept\n' >> "/etc/default/$name"
mkdir -p "/usr/lib/$name/app/node_modules/left-behind"
printf 'module.exports = 1;\n' > "/usr/lib/$name/app/node_modules/left-behind/index.js"

printf '\n=== upgrade\n'
dpkg --install "$new"

check "the administrator's configuration survives" "grep -q ADMIN_SETTING /etc/default/$name"
check "installed dependencies survive"             "test -f /usr/lib/$name/app/node_modules/left-behind/index.js"
check "the account is not recreated"               "getent passwd $name"
check "the payload is the new version"             "dpkg-query -W -f='\${Version}' $name | grep -q 0.2.0"

printf '\n=== disable, then upgrade again\n'
# `deb-systemd-helper` records the choice even with no init running; `systemctl enable` cannot.
deb-systemd-helper disable "$name.service" >/dev/null 2>&1 || true
dpkg --install "$new"
check "a disabled unit stays disabled across an upgrade" \
      "! deb-systemd-helper --quiet is-enabled $name.service"

printf '\n'
[ "$fail" -eq 0 ] && echo "ALL UPGRADE SCENARIOS PASSED" || echo "SOME UPGRADE SCENARIOS FAILED"
exit "$fail"
