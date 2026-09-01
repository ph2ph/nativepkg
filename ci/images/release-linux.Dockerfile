# The image that builds release binaries: static musl, so one Linux binary runs on any
# distribution regardless of its glibc. Every line below was earned by a failed build:
#
#   build-essential  host build scripts (thiserror, rustix, …) link against glibc and need its
#                    crt files — bare `gcc` on a slim image lacks libc6-dev and fails with
#                    "cannot open crtn.o";
#   musl-tools       musl-gcc and the musl headers, which zstd-sys and liblzma-sys compile C
#                    against — without them: "stddef.h: No such file or directory";
#   gcc-aarch64-*    the cross C compiler for the arm64 target.
#
# The musl linker is scoped to the target in .github/workflows/release.yml, never set globally: a global
# CC=musl-gcc links the host build scripts against musl and they fail to load.
FROM rust:1.95-slim
RUN apt-get update \
 && apt-get install --yes --no-install-recommends build-essential musl-tools gcc-aarch64-linux-gnu file \
 && rm -rf /var/lib/apt/lists/* \
 && rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl
