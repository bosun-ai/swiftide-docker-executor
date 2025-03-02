version :=  `cargo metadata --format-version=1 --no-deps | jq '.packages[0].version'`

docker-build-service:
  docker build  -t bosunai/swiftide-docker-service:{{version}} -f swiftide-docker-service/Dockerfile .

docker-run-service: docker-build-service
  docker run -p 50051:50051 bosunai/swiftide-docker-service:{{version}} swiftide-docker-service

