# Ubuntu's current LTS. The old matrix stopped at trusty and xenial.
FROM ubuntu:noble

RUN apt-get update \
 && apt-get install --yes --no-install-recommends \
      systemd systemd-sysv procps lintian nodejs ca-certificates \
 && rm -rf /var/lib/apt/lists/*

CMD ["/bin/bash"]
