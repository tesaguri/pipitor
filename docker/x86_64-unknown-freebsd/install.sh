set -ex

temp="$(mktemp -d)"
pushd "$temp"

curl -sSfLO 'https://sqlite.org/2020/sqlite-autoconf-3320300.tar.gz'
tar -xzf 'sqlite-autoconf-3320300.tar.gz'
cd 'sqlite-autoconf-3320300'
./configure --prefix='/usr/local/x86_64-unknown-freebsd10'
make install "-j$(nproc)"

popd
rm -rf "$temp"
rm "$0"
