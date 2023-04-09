# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.2] - 2023-04-05

### Fixed

- Do not panic if an entity was already despawned on client.
- Fix panic that could occur when RenetServer or RenetClient resources are removed.

## [0.2.1] - 2023-04-02

### Fixed

- Fix incorrect last tick detection.

## [0.2.0] - 2023-04-01

### Changed

- Use `#[reflect(MapEntities)]` from Bevy 0.10.1 instead of custom `#[reflect(MapEntity)]`.

### Fixed

- Fix tick checks after overflow.

## [0.1.0] - 2023-03-28

Initial release after separation from [lifescape](https://github.com/lifescapegame/lifescape).

[unreleased]: https://github.com/lifescapegame/bevy_replicon/compare/v0.2.2...HEAD
[0.2.2]: https://github.com/lifescapegame/bevy_replicon/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/lifescapegame/bevy_replicon/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/lifescapegame/bevy_replicon/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/lifescapegame/bevy_replicon/releases/tag/v0.1.0
