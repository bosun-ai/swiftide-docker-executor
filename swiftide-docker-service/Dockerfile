# This dockerfile is used to build the swiftide-docker-service image
FROM rust:1.87-slim as builder

RUN apt-get update && apt-get install -y --no-install-recommends \
  protobuf-compiler \
  libprotobuf-dev \
  pkg-config libssl-dev iputils-ping \
  make \
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

# Installs some common dependencies we need for kwaak :')'
# They can be added to the grpc service later
# RUN apt-get update && apt-get install -y --no-install-recommends \
#   ripgrep fd-find \
#   && apt-get clean \
#   && rm -rf /var/lib/apt/lists/* \
#   && cp /usr/bin/fdfind /usr/bin/fd
RUN apk add --no-cache gcompat ripgrep fd

WORKDIR /app

EXPOSE 50051
CMD ["swiftide-docker-service"]
