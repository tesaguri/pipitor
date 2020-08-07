#!/usr/bin/env bash

set -ex

main() {
    local arch="$1"

    local dependencies=(
        ca-certificates
        curl
        gzip
        make
    )
    local purge_list=()

    apt-get update
    apt-get install --no-install-recommends --assume-yes libsqlite3-dev
    for dep in "${dependencies[@]}"; do
        if ! dpkg -L "$dep"; then
            apt-get install --no-install-recommends --assume-yes "$dep"
            purge_list+=( "$dep" )
        fi
    done

    temp="$(mktemp -d)"
    pushd "$temp"

    curl -sSfLO 'https://sqlite.org/2020/sqlite-autoconf-3320300.tar.gz'
    tar -xzf 'sqlite-autoconf-3320300.tar.gz'
    cd 'sqlite-autoconf-3320300'
    ./configure --prefix="/usr/local/$arch-unknown-freebsd10"
    make install "-j$(nproc)"

    popd

    if (( ${#purge_list[@]} )); then
        apt-get purge --assume-yes --auto-remove "${purge_list[@]}"
    fi

    rm -rf "$temp"
    rm "$0"
}

main "$@"
