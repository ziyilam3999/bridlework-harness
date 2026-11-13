# Image for the clean-install container run. The official rust image (pinned by digest) is a minimal profile without clippy, and
# core/rust/rust-toolchain.toml lists clippy, so an offline build would try to download it. Build time only uses the network.
# CARGO_HOME and RUSTUP_HOME stay at the image's own /usr/local paths (outside the test HOME) and are made writable for uid 1000.
FROM rust@sha256:e6f2cff35d0f309acf8433b4f05878ed77092602c5065cc81801177da0bb3a9c
RUN rustup component add clippy && chmod -R a+rwX /usr/local/cargo /usr/local/rustup
