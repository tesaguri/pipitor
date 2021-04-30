FROM ghcr.io/tesaguri/cross:armv7-unknown-linux-gnueabihf

RUN dpkg --add-architecture armhf && \
    apt-get update && \
    apt-get install --assume-yes libsqlite3-dev libsqlite3-dev:armhf libssl-dev:armhf

ENV PKG_CONFIG_ALLOW_CROSS_armv7_unknown_linux_gnueabihf=1 \
    PKG_CONFIG_PATH_armv7_unknown_linux_gnueabihf='/usr/lib/arm-linux-gnueabihf/pkgconfig'
