# Arch. Rolling, so the image is rebuilt rather than pinned; `pacman -Syu` in the same layer as
# the install keeps the two from disagreeing about the package database.
FROM archlinux:base

RUN pacman -Syu --noconfirm --needed systemd procps-ng nodejs \
 && pacman -Scc --noconfirm

CMD ["/bin/bash"]
