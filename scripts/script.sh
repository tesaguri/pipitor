# This script takes care of testing the crate.

set -ex

# if [ -z "$TARGET" ]; then
#   cargo fmt -- --check
# fi

cargo check --verbose --features 'openssl-vendored sqlite-bundled'
