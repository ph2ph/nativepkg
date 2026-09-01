#!/bin/bash
# Installs a package built from tests/fixtures/hello-svc and proves the service it ships runs:
# no init in a container, so the unit's ExecStart is executed the way systemd would, as that
# user, from that directory, and must write to the daemon's log.
#
#   $1 = package   $2 = package name   $3 = install root given at build time
set -u
pkg="$1"; name="$2"; root="$3"; fail=0
check() { if eval "$2" >/dev/null 2>&1; then printf '  ok    %s\n' "$1"; else printf '  FAIL  %s\n' "$1"; fail=1; fi; }

if command -v dpkg >/dev/null; then dpkg -i "$pkg" >/dev/null 2>&1
elif command -v rpm >/dev/null; then rpm -i --nodeps "$pkg" >/dev/null 2>&1
else pacman -U --noconfirm --nodeps "$pkg" >/dev/null 2>&1; fi || { echo "  FAIL  install"; exit 1; }
echo "  ok    install"

check "payload under the requested root"     "test -x $root/$name/app/index.js"
check "the command is on PATH"               "test -L /usr/bin/hello && test -x \$(readlink -f /usr/bin/hello)"
check "the service account exists"           "id hello"
unit=/usr/lib/systemd/system/$name.service
check "the unit exists"                      "test -f $unit"
exec_start=$(grep -oE '^ExecStart=.*' "$unit" | cut -d= -f2-)
check "ExecStart is under the requested root" "case '$exec_start' in $root/*) true;; *) false;; esac"

# Run the unit's command as its user for three seconds, as systemd would.
mkdir -p /var/log/$name && chown hello:hello /var/log/$name
runuser -u hello -- sh -c "cd $root/$name && timeout 3 $exec_start" >/dev/null 2>&1
check "the service ran and wrote its log"    "test \$(wc -l < /var/log/$name/hello.log) -ge 2"
check "the log says where it ran from"       "grep -q 'cwd=$root/$name' /var/log/$name/hello.log"

[ "$fail" -eq 0 ] && echo "SERVICE RUNS" || echo "SERVICE DOES NOT RUN"
exit "$fail"
