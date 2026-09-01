# The RPM family. There was no RPM target at all before: the old tool built only `.deb`.
FROM fedora:41

RUN dnf install --assumeyes --setopt=install_weak_deps=False \
      systemd procps-ng rpm nodejs \
 && dnf clean all

CMD ["/bin/bash"]
