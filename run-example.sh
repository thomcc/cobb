#!/usr/bin/env bash

env RUSTFLAGS="-Zsanitizer=thread" \
    cargo +nightly run -Zbuild-std --target=x86_64-apple-darwin --all-features --release --example "$0"

