# Build stage
FROM rust:1-slim-bookworm AS builder

WORKDIR /lazyjq
COPY . /lazyjq
RUN cargo build --release

# Final stage
FROM debian:bookworm-slim

COPY --from=builder /lazyjq/target/release/lazyjq /bin/lazyjq

ENTRYPOINT ["/bin/lazyjq"]
