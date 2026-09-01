# Debian's current stable. The bash implementation's matrix targeted wheezy, jessie and
# stretch, all long past end of life — the newest of them lost security support in 2022 — so
# the suite was proving the tool worked on systems nobody could run.
FROM debian:trixie-slim

# `systemd` is present so the maintainer scripts can be exercised for real rather than skipped;
# `lintian` and `piuparts` are the policy gates. `procps` gives the scenarios a way to see
# whether anything is actually running.
RUN apt-get update \
 && apt-get install --yes --no-install-recommends \
      systemd systemd-sysv procps lintian piuparts debootstrap shellcheck nodejs ca-certificates adduser \
 && rm -rf /var/lib/apt/lists/*

# A container has no init, so `systemctl` would fail differently from a real machine. The
# scenarios that need one run under `systemd` as PID 1; the rest assert the install succeeds
# and starts nothing, which is the behaviour a build chroot must have.
CMD ["/bin/bash"]
