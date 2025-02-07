version :=  `cargo metadata --format-version=1 --no-deps | jq '.packages[0].version'`

docker-build-service:
  docker build  -t bosunai/swiftide-docker-service:{{version}} -f swiftide-docker-service/Dockerfile .
