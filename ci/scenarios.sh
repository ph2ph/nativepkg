#!/bin/bash
# Package-level scenarios, run inside a distribution container against a package built outside
# it: what `dpkg`, `rpm` or `pacman` actually does with the archive.
set -euo pipefail

pkg="${1:?usage: scenarios.sh <package> <name>}"
name="${2:?usage: scenarios.sh <package> <name>}"
fail=0

note() { printf '\n=== %s\n' "$1"; }
check() {
  if eval "$2" >/dev/null 2>&1; then printf '  ok    %s\n' "$1"
  else printf '  FAIL  %s\n' "$1"; fail=1; fi
}

note "install"
if command -v dpkg >/dev/null 2>&1; then
  dpkg --install "$pkg" || { echo "  FAIL install exited $?"; exit 1; }
elif command -v rpm >/dev/null 2>&1; then
  # `--nodeps`: resolving dependencies needs the network, and installing without one is the
  # point. So this pipeline never verifies the declared dependencies, only that the package
  # installs and behaves once they are present.
  rpm --install --nodeps "$pkg"
elif command -v pacman >/dev/null 2>&1; then
  # Same trade as rpm's `--nodeps` above.
  pacman --upgrade --noconfirm --nodeps "$pkg"
else
  echo "  no package manager present"; exit 1
fi

check "the payload is installed"        "test -d /usr/lib/$name"
check "the wrapper exists"              "test -f /usr/lib/$name/bin/$name"
check "the wrapper is executable"       "test -x /usr/lib/$name/bin/$name"
check "the /usr/bin link resolves"      "test -e /usr/bin/$name"
check "the service account exists"      "getent passwd $name"
check "the group exists"                "getent group $name"
check "the unit is installed"           "test -f /usr/lib/systemd/system/$name.service"
check "the defaults file is installed"  "test -f /etc/default/$name"

# A container has no running init. The scripts must notice and do nothing rather than fail —
# the same condition a build chroot presents.
check "nothing was started without an init" "! systemctl is-active --quiet $name 2>/dev/null"

note "reinstall over itself"
if command -v dpkg >/dev/null 2>&1; then
  dpkg --install "$pkg"
  check "the payload survives a reinstall" "test -f /usr/lib/$name/bin/$name"
fi

note "remove and purge"
if command -v dpkg >/dev/null 2>&1; then
  dpkg --remove "$name"
  check "the payload is gone"      "! test -d /usr/lib/$name/app"
  check "the account survives a remove" "getent passwd $name"
  dpkg --purge "$name"
  check "the account is gone after purge" "! getent passwd $name"
  check "the defaults file is gone"       "! test -f /etc/default/$name"
elif command -v rpm >/dev/null 2>&1; then
  rpm --erase --nodeps "$name"
  check "the payload is gone" "! test -d /usr/lib/$name/app"
elif command -v pacman >/dev/null 2>&1; then
  pacman --remove --noconfirm --nodeps "$name"
  check "the payload is gone" "! test -d /usr/lib/$name/app"
fi

printf '\n'
[ "$fail" -eq 0 ] && echo "ALL SCENARIOS PASSED" || echo "SOME SCENARIOS FAILED"
exit "$fail"
