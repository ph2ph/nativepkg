#!/bin/bash
# Runs INSIDE a container booted with systemd as PID 1: installs the package, lets the
# maintainer scripts enable and start, then asks systemd — not the filesystem — what happened.
# The only way to see whether postinst's deb-systemd-helper/preset calls took effect.
#
#   $1 = package (v1)   $2 = package (v2, for the upgrade half)   $3 = unit name
set -u
pkg1="$1"; pkg2="$2"; unit="$3"; fail=0
check() { if eval "$2" >/dev/null 2>&1; then printf '  ok    %s\n' "$1"; else printf '  FAIL  %s  [%s]\n' "$1" "$(eval "$2" 2>&1 | tail -1)"; fail=1; fi; }
install() {
  # `--oldpackage`: the third install puts v1 back over v2. Without it `rpm -U` refuses the
  # downgrade silently behind the redirect, and the "disable survives an upgrade" checks pass
  # against a unit nothing touched.
  if command -v dpkg >/dev/null; then dpkg -i "$1"
  elif command -v rpm >/dev/null; then rpm -U --nodeps --oldpackage "$1"
  else pacman -U --noconfirm --nodeps "$1"; fi >/dev/null 2>&1
}

for i in $(seq 1 30); do systemctl is-system-running 2>/dev/null | grep -qE "running|degraded" && break; sleep 1; done

# Debian's Docker images ship a policy-rc.d returning 101 ("never start services"), which the
# maintainer scripts honour. A real machine has none, and this script is about a real machine.
rm -f /usr/sbin/policy-rc.d
check "systemd is PID 1"                     "test \$(ps -p 1 -o comm=) = systemd"

echo "--- install v1: postinst must enable and start, on its own"
install "$pkg1" || { echo "  FAIL  install"; exit 1; }
check "unit is known to systemd"             "systemctl cat $unit"
check "unit is enabled (postinst enabled it)" "systemctl is-enabled $unit"
sleep 2
check "unit is active (postinst started it)" "systemctl is-active $unit"
check "the daemon wrote to its log"          "test -s /var/log/${unit%.service}/hello.log"
check "journal shows the unit starting"      "journalctl -u $unit --no-pager | grep -qiE 'started|starting'"
main_pid=$(systemctl show -p MainPID --value $unit)
check "systemd tracks a live MainPID"        "test $main_pid -gt 0 && kill -0 $main_pid"

echo "--- upgrade v1 -> v2: the running service must be restarted, not left stale"
install "$pkg2" || { echo "  FAIL  upgrade"; exit 1; }
sleep 2
check "still active after upgrade"           "systemctl is-active $unit"
check "restarted: MainPID changed"           "test \$(systemctl show -p MainPID --value $unit) -ne $main_pid"
check "still enabled after upgrade"          "systemctl is-enabled $unit"

echo "--- administrator disables, then upgrades again: the choice must survive"
systemctl disable --now $unit >/dev/null 2>&1
# v1 back over v2: to the maintainer scripts this is an upgrade transaction like any other.
install "$pkg1" || { echo "  FAIL  reinstall (the upgrade transaction did not run)"; fail=1; }
sleep 2
check "a disabled unit stays disabled across an upgrade" "! systemctl is-enabled $unit"
check "and is not started behind the administrator's back" "! systemctl is-active $unit"

echo "--- remove: the unit must be stopped and gone"
systemctl enable --now $unit >/dev/null 2>&1; sleep 1
if command -v dpkg >/dev/null; then dpkg -r "${unit%.service}"; elif command -v rpm >/dev/null; then rpm -e --nodeps "${unit%.service}"; else pacman -R --noconfirm --nodeps "${unit%.service}"; fi >/dev/null 2>&1
sleep 1
check "unit is stopped after removal"        "! systemctl is-active $unit"
check "unit file is gone after removal"      "! test -f /usr/lib/systemd/system/$unit"
if command -v dpkg >/dev/null; then
  # On remove, debhelper's postrm masks the unit until purge; purge unmasks and forgets it.
  check "debian: unit is masked after remove"  "systemctl is-enabled $unit 2>/dev/null | grep -q masked"
  dpkg -P "${unit%.service}" >/dev/null 2>&1; sleep 1
  check "debian: unit is gone after purge"     "! systemctl list-unit-files $unit --no-pager --no-legend | grep -q ."
  check "debian: no enablement state left"     "! ls /var/lib/systemd/deb-systemd-helper-enabled 2>/dev/null | grep -q ${unit%.service}"
else
  check "systemd no longer lists it"           "! systemctl list-unit-files $unit --no-pager --no-legend | grep -q ."
fi

echo "--- an administrator's preset policy, placed before installation, must win"
# What the scripts use `preset` for: a package that called `enable` would pass every check
# above and fail this one.
mkdir -p /etc/systemd/system-preset
echo "disable $unit" > /etc/systemd/system-preset/00-administrator.preset
install "$pkg1" || { echo "  FAIL  install under administrator policy"; fail=1; }; sleep 1
check "unit not enabled against the administrator's policy" "! systemctl is-enabled $unit"
if command -v dpkg >/dev/null; then dpkg -P "${unit%.service}"; elif command -v rpm >/dev/null; then rpm -e --nodeps "${unit%.service}"; else pacman -R --noconfirm --nodeps "${unit%.service}"; fi >/dev/null 2>&1
rm -f /etc/systemd/system-preset/00-administrator.preset

[ "$fail" -eq 0 ] && echo "SYSTEMD LIFECYCLE OK" || echo "SYSTEMD LIFECYCLE FAILED"
exit "$fail"
