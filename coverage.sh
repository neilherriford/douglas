#!/bin/bash
set -e

if [[ -z "$1" && -z "$2" ]]; then
    tests="--all"
else
  tests="--workspace -p $1 $2"
fi


# Clean up previous run
rm -f *.profraw
rm -rf target/coverage

# Set environment
export CARGO_INCREMENTAL=0
export RUSTFLAGS="-Cinstrument-coverage"
export LLVM_PROFILE_FILE="cargo-test-%p-%m.profraw"

# Run tests
cargo test $tests -- --nocapture

# Generate coverage
grcov . \
  --binary-path ./target/debug/deps/ \
  -s . \
  -t html \
  --branch \
  --ignore-not-existing \
  --ignore '../*' \
  --ignore "/*" \
  --ignore 'target/*' \
  --excl-start '^#\[cfg\(test\)\]' \
  --excl-stop '^}$' \
  -o target/coverage/

# Clean up profile files
find . -type f -name '*.profraw' -print0 | xargs -0 rm
