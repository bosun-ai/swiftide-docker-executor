version :=  `cargo metadata --format-version=1 --no-deps | jq -r '.packages[0].version'`

docker-build-service:
  docker build -t bosunai/swiftide-docker-service:{{version}} -t bosunai/swiftide-docker-service:latest -f swiftide-docker-service/Dockerfile .

docker-run-service: docker-build-service
  docker run -p 50051:50051 bosunai/swiftide-docker-service:{{version}} swiftide-docker-service

test *args: docker-build-service
  RUST_LOG=swiftide_docker_executor=debug cargo nextest run {{args}} --no-fail-fast

