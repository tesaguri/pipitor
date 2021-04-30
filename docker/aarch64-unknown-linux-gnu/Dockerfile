FROM ghcr.io/tesaguri/cross:aarch64-unknown-linux-gnu

RUN dpkg --add-architecture arm64 && \
    apt-get update && \
    apt-get install --assume-yes libsqlite3-dev libsqlite3-dev:arm64 libssl-dev:arm64

ENV PKG_CONFIG_ALLOW_CROSS_aarch64_unknown_linux_gnu=1 \
    PKG_CONFIG_PATH_aarch64_unknown_linux_gnu='/usr/lib/aarch64-linux-gnu/pkgconfig'
