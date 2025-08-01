# This dockerfile is used to build the swiftide-docker-service image
FROM rust:1.88-slim as builder

RUN apt-get update && apt-get install -y --no-install-recommends \
  protobuf-compiler \
  libprotobuf-dev \
  pkg-config libssl-dev iputils-ping \
  make \
  # Avoid flaky symlinked binaries from alpine
  fd-find \
  ripgrep \
  && apt-get clean \
  && rm -rf /var/lib/apt/lists/*

COPY . /app
WORKDIR /app

RUN \
  --mount=type=cache,target=/app/target/ \
  --mount=type=cache,target=/usr/local/cargo/git/db \
  --mount=type=cache,target=/usr/local/cargo/registry/ \
  cargo build --release -p swiftide-docker-service \
  && cp target/release/swiftide-docker-service /usr/bin/swiftide-docker-service

FROM alpine as runtime

COPY --from=builder /usr/bin/swiftide-docker-service /usr/bin/swiftide-docker-service
COPY --from=builder /usr/bin/fdfind /usr/bin/fd
COPY --from=builder /usr/bin/rg /usr/bin/rg

WORKDIR /app

RUN apk add gcompat libgcc

EXPOSE 50051
CMD ["swiftide-docker-service"]
