FROM rust:1.44.1 as builder

WORKDIR /work
COPY / /work/
RUN cargo build --release
RUN cp -R /work/target/release /build

FROM scratch
COPY --from=builder /work/target/release/chunk-search-rs /build/chunk-search-rs
