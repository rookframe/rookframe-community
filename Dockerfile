FROM rust:1.97.1-bookworm AS build
WORKDIR /source
COPY Cargo.toml Cargo.lock build.rs ./
COPY migrations ./migrations
COPY src ./src
RUN cargo build --release --locked
FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /source/target/release/rookframe-community /app/community
USER 65532:65532
ENTRYPOINT ["/app/community"]
