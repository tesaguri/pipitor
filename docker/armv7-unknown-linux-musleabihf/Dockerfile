FROM rustembedded/cross:armv7-unknown-linux-musleabihf-0.2.1

RUN dpkg --add-architecture arm-linux-musleabihf  && \
    apt-get update && \
    apt-get install --assume-yes libsqlite3-dev:arm-linux-musleabihf
