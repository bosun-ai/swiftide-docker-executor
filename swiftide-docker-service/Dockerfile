# This dockerfile is used to build the swiftide-docker-service image
FROM rust:1.83-slim as builder

RUN apt-get update && apt-get install -y --no-install-recommends \
  protobuf-compiler \
  libprotobuf-dev \
  pkg-config libssl-dev iputils-ping \
  make

COPY . /app
WORKDIR /app

RUN cargo build --release -p swiftide-docker-service

RUN cp target/release/swiftide-docker-service /usr/bin/swiftide-docker-service

FROM ubuntu:latest

COPY --from=builder /usr/bin/swiftide-docker-service /usr/bin/swiftide-docker-service

# Installs some common depedencies we need for kwaak :')'
# They can be added to the grpc service later
RUN apt-get update && apt-get install -y --no-install-recommends \
  ripgrep fd-find git ssh curl \
  && rm -rf /var/lib/apt/lists/* \
  && cp /usr/bin/fdfind /usr/bin/fd

