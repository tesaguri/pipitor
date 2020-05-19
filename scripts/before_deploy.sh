# This script takes care of building the crate and packaging it for release.

PKG_NAME="pipitor"

set -ex

main() {
  local src=$(pwd) \
    stage=$(mktemp -d) \
    cargo=

  if [ "$TRAVIS_OS_NAME" = linux ]; then
    cargo='cross'
  else
    cargo='cargo'
  fi

  test -f Cargo.lock || cargo generate-lockfile

  $cargo rustc --bin $PKG_NAME --target $TARGET --release --no-default-features --features 'rustls sqlite-bundled' -- -C lto -C codegen-units=1
  cp target/$TARGET/release/$PKG_NAME $stage/

  cd $stage
  tar czf $src/$CRATE_NAME-$TRAVIS_TAG-$TARGET.tar.gz *
  cd $src

  rm -rf $stage
}

if [ -n "$TARGET" ]; then
  main
fi
