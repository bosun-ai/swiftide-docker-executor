# This dockerfile is used to build the swiftide-docker-service image
# Using Alpine + musl for fully static binaries (no glibc dependency)
FROM rust:1.89-alpine as builder

RUN apk add --no-cache \
  musl-dev \
  protobuf-dev \
  protoc \
  pkgconfig \
  openssl-dev \
  openssl-libs-static \
  make \
  perl

# Install static versions of fd and ripgrep
RUN apk add --no-cache fd ripgrep

COPY . /app
WORKDIR /app

ENV OPENSSL_STATIC=1
ENV OPENSSL_LIB_DIR=/usr/lib
ENV OPENSSL_INCLUDE_DIR=/usr/include

RUN \
  --mount=type=cache,target=/app/target/ \
  --mount=type=cache,target=/usr/local/cargo/git/db \
  --mount=type=cache,target=/usr/local/cargo/registry/ \
  cargo build --release -p swiftide-docker-service \
  && cp target/release/swiftide-docker-service /usr/bin/swiftide-docker-service

FROM alpine as runtime

COPY --from=builder /usr/bin/swiftide-docker-service /usr/bin/swiftide-docker-service
COPY --from=builder /usr/bin/fd /usr/bin/fd
COPY --from=builder /usr/bin/rg /usr/bin/rg

WORKDIR /app

EXPOSE 50051
CMD ["swiftide-docker-service"]
