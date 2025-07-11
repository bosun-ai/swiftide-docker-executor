# Changelog

All notable changes to this project will be documented in this file.

## [0.11.0] - 2025-07-04

### 🐛 Bug Fixes

- Avoid accidental cleanup if executor is still used after file loader

### ⚙️ Miscellaneous Tasks

- Add borrowed variant of file loader (#87)

<!-- generated by git-cliff -->
## [0.10.0] - 2025-06-30

### ⚙️ Miscellaneous Tasks

- Fix copy the wrong pasta
- Update for swiftide 0.28 (#88)

<!-- generated by git-cliff -->
## [0.9.0] - 2025-06-26

### 🚀 Features

- Allow executor to be used as file loader for indexing remotely (#82)
- Faster builds with buildkit (#79)

### 🎨 Styling

- Clippy

<!-- generated by git-cliff -->
## [0.8.0] - 2025-06-23

### 🚀 Features

- Support setting the container user

<!-- generated by git-cliff -->
## [0.7.6] - 2025-06-11

### 🚀 Features

- Support alpine based images

<!-- generated by git-cliff -->
## [0.7.5] - 2025-06-11

### 🚀 Features

- Support existing image without build and a much smaller executor base image (250mb -> 27mb) (#75)

### ⚙️ Miscellaneous Tasks

- Update swiftide and deps (#76)

<!-- generated by git-cliff -->
<!-- generated by git-cliff -->
## [0.7.3] - 2025-04-16

### ⚙️ Miscellaneous Tasks

- Update swiftide

<!-- generated by git-cliff -->
## [0.7.2] - 2025-04-11

### ⚙️ Miscellaneous Tasks

- Update Swiftide

<!-- generated by git-cliff -->
## [0.7.1] - 2025-04-08

### ⚙️ Miscellaneous Tasks

- Update deps (#67)

<!-- generated by git-cliff -->
## [0.7.0] - 2025-03-28

### 🚀 Features

- Use a temporary dockerfile and add it to the context directly (#66)

### 🐛 Bug Fixes

- Correctly set the image builder version
- Disable image pull if present locally
- Test, build and perf tuning
- Enable pull again
- Buildkit behind feature flag (#65)

<!-- generated by git-cliff -->
## [0.6.7] - 2025-03-06

### ⚙️ Miscellaneous Tasks

- Update Swiftide to 0.22

<!-- generated by git-cliff -->
## [0.6.6] - 2025-03-02

### 🚀 Features

- Stream command output to container output with tracing ([#58](https://github.com/bosun-ai/swiftide-docker-executor/pull/58))
- Faster builds with buildkit ([#59](https://github.com/bosun-ai/swiftide-docker-executor/pull/59))

<!-- generated by git-cliff -->
## [0.6.5] - 2025-02-26

### 🐛 Bug Fixes

- Ignore failed symlinks in context building and improve io errors (#51)

<!-- generated by git-cliff -->
## [0.6.4] - 2025-02-25

### ⚙️ Miscellaneous Tasks

- Update swiftide

<!-- generated by git-cliff -->
<!-- generated by git-cliff -->
## [0.6.2] - 2025-02-13

### ⚙️ Miscellaneous Tasks

- Update Swiftide and other deps

<!-- generated by git-cliff -->
## [0.6.1] - 2025-02-13

### 💼 Other

- Ensure errors always show underlying error

### ⚙️ Miscellaneous Tasks

- Connecting to docker client error is now transparent

<!-- generated by git-cliff -->
## [0.6.0] - 2025-02-12

### 🚀 Features

- Better errors when starting fails

### 🐛 Bug Fixes

- Improve polling while waiting for container to start

### ⚙️ Miscellaneous Tasks

- Add regression test for previous kwaak dockerfile

<!-- generated by git-cliff -->
## [0.5.0] - 2025-02-09

### 🚀 Features

- Grpc based executor (#39)

### 🐛 Bug Fixes

- All properties in virtual manifest

<!-- generated by git-cliff -->
<!-- generated by git-cliff -->
## [0.4.1] - 2025-01-18

### 🐛 Bug Fixes

- Handle symlinks correctly during context building (#29)

<!-- generated by git-cliff -->
## [0.4.0] - 2025-01-18

### 🐛 Bug Fixes

- Remove working directory and use a lazy shared docker connection (#27)

<!-- generated by git-cliff -->
<!-- generated by git-cliff -->
## [0.3.0] - 2025-01-17

### 🐛 Bug Fixes

- Ensure git is in context (#23)

<!-- generated by git-cliff -->
## [0.2.3] - 2025-01-16

### 🐛 Bug Fixes

- Return proper build errors when image is incorrect (#20)
- *(ci)* Add timeout to pipelines
- *(ci)* Remove concurrency limit and disk space action

<!-- generated by git-cliff -->
## [0.2.2] - 2025-01-11

### ⚙️ Miscellaneous Tasks

- Update Cargo.lock dependencies

<!-- generated by git-cliff -->
## [0.2.1] - 2025-01-11

### 🚀 Features

- Remove container with force in favour of kill (#13)

### 🐛 Bug Fixes

- Use the same socket for connecting and mounting

<!-- generated by git-cliff -->
## [0.2.0] - 2025-01-08

### 🚀 Features

- Add helpers to check if the executor is properly up (#6)

### 🐛 Bug Fixes

- Reliably determine docker socket (#5)
- Ovewrite cmd with sleep infinity if its present (by accident) (#7)
- *(ci)* Move coverage file to the correct folder :>

### ⚙️ Miscellaneous Tasks

- Loosen up and update deps
- Add test coverage workflow (#9)

<!-- generated by git-cliff -->
## [0.1.1] - 2025-01-06

### 🚀 Features

- Use buildkit when building images (#3)

### ⚙️ Miscellaneous Tasks

- Release v0.1.0 (#1)

<!-- generated by git-cliff -->
## [0.1.0] - 2025-01-04

### 🚀 Features

- Initial commit

### 🐛 Bug Fixes

- Upstream tests from kwaak
- Docs and From for Arc<dyn ToolExecutor>
- Add missing metadata for Cargo.toml

### 📚 Documentation

- The basics

### ⚙️ Miscellaneous Tasks

- Add github actions
- Set triggers on main

<!-- generated by git-cliff -->
