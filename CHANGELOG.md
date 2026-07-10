# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/).














## [0.5.2](https://github.com/rvben/n8nc/compare/v0.5.1...v0.5.2) - 2026-07-10

### Added

- **workflow**: execute through n8n's internal REST when no backend is configured ([797a5d1](https://github.com/rvben/n8nc/commit/797a5d1797e88454d32e5a0749a0a2adf920597b))
- **runs**: add stats --status, --by workflow, and cadence gaps ([702efaa](https://github.com/rvben/n8nc/commit/702efaac44bf26d13ebb6273486679f16af19e3c))
- **node**: add set-remote for one-shot edits on untracked workflows ([64d0f2a](https://github.com/rvben/n8nc/commit/64d0f2acb5588819f93a939a7eff831792c5cf03))
- **get**: add --node, --nodes and --connections projections ([c88d458](https://github.com/rvben/n8nc/commit/c88d458c919498e141c40a8f888d64c718a54dce))
- **lint**: add params-match-type-version rule ([4d962f3](https://github.com/rvben/n8nc/commit/4d962f328302075727f0e2fa585bac81e7f55101))

### Fixed

- **workflow**: do not exit 0 when an execution did not succeed ([b0fbf4c](https://github.com/rvben/n8nc/commit/b0fbf4c4c6db8ce21887ce2f13fa2d034c0722e0))
- **remote**: stop canonicalization from deleting node parameters ([a43393c](https://github.com/rvben/n8nc/commit/a43393c1fd5dcc7679249a42d436a576d1e84f1a))
- **runs**: stop shipping the run payload twice in get --details ([0c3e2c0](https://github.com/rvben/n8nc/commit/0c3e2c0b3930507da8d5cfd8eb4ff0ed4654e61d))

## [0.5.1](https://github.com/rvben/n8nc/compare/v0.5.0...v0.5.1) - 2026-07-10

### Added

- **node**: read `node set` values from a file ([110155d](https://github.com/rvben/n8nc/commit/110155d3963fad99f370e25c675e2daf77df101f))
- **runs**: add --summary, --node, and --explain projections ([b9232d0](https://github.com/rvben/n8nc/commit/b9232d04468ae9c4256d88d722fef602d3bcf651))

### Fixed

- **api**: stop over-reporting truncation and warn in ls text output ([afda670](https://github.com/rvben/n8nc/commit/afda670d3d9f482c90d20328fd6996f9d2f5fe32))
- **api**: paginate past the page cap instead of truncating in silence ([5614023](https://github.com/rvben/n8nc/commit/56140233a3da4d2df619da47e503ee32462a2151))

## [0.5.0](https://github.com/rvben/n8nc/compare/v0.4.7...v0.5.0) - 2026-07-01

### Added

- **secret**: add `secret extract` to move inline header tokens to credentials ([1d27ace](https://github.com/rvben/n8nc/commit/1d27ace4fcd85b6c064ce1c71550d4a262d6afb8))
- **cli**: add `diff --remote` alias and `push --verify` drift check ([f068e2d](https://github.com/rvben/n8nc/commit/f068e2dc8890203dae49cb70c67b3cfa2fd13061))

### Fixed

- **push**: verify drift against the pushed payload, not the full workflow ([6e8aae0](https://github.com/rvben/n8nc/commit/6e8aae0ec20830857276368b039fac13fc74a913))
- **status**: treat sensitive-data warnings as advisory, not invalid ([33fd3df](https://github.com/rvben/n8nc/commit/33fd3df4ce1a8f035ac6ec59a43e8ce65c28a94e))
- **node**: resolve nodes by id as well as display name ([3b39bbd](https://github.com/rvben/n8nc/commit/3b39bbdb681ebaa5d9a7e208c168e8ecaeb5c16c))
- **node**: preserve integer values for `node set --number` ([df02428](https://github.com/rvben/n8nc/commit/df02428a4d20fc8933ddaed605c9192cae33de77))

## [0.4.7](https://github.com/rvben/n8nc/compare/v0.4.6...v0.4.7) - 2026-07-01

### Fixed

- **push,diff**: accept workflow id/slug and strip API-incompatible settings ([5140b9f](https://github.com/rvben/n8nc/commit/5140b9fc6d5c2b500a83f98f3d1402803afc01e3))

## [0.4.6](https://github.com/rvben/n8nc/compare/v0.4.5...v0.4.6) - 2026-06-20

### Added

- **schema**: fill missing output_fields for 32 commands ([eae1aae](https://github.com/rvben/n8nc/commit/eae1aae7d7522f041b5985c7f722465113b351a8))

## [0.4.5](https://github.com/rvben/n8nc/compare/v0.4.4...v0.4.5) - 2026-06-20

### Added

- **schema**: declare exit-code outcomes in the contract ([be3e79a](https://github.com/rvben/n8nc/commit/be3e79af0abad27d717f1d9b84d43611689cf2cd))

## [0.4.4](https://github.com/rvben/n8nc/compare/v0.4.3...v0.4.4) - 2026-06-11

### Added

- **clispec**: achieve full v0.2 compliance (24/24) ([535279e](https://github.com/rvben/n8nc/commit/535279eaf5b9b1afe90a8dd16230d69d2c6b6737))

### Fixed

- **lint**: resolve clippy warnings introduced by Rust 1.95 ([7c192e2](https://github.com/rvben/n8nc/commit/7c192e287e91c3c8653a524c3745de0008a6e2dc))

## [0.4.3](https://github.com/rvben/n8nc/compare/v0.4.2...v0.4.3) - 2026-04-03

### Added

- **init**: add interactive init with credential validation and next steps ([7b74ef0](https://github.com/rvben/n8nc/commit/7b74ef0d362122bf8442dd2c56b60b9cea06a1a0))

## [0.4.2](https://github.com/rvben/n8nc/compare/v0.4.1...v0.4.2) - 2026-04-03

## [0.4.1](https://github.com/rvben/n8nc/compare/v0.4.0...v0.4.1) - 2026-04-03

### Added

- add colored terminal output with owo-colors ([d12a837](https://github.com/rvben/n8nc/commit/d12a837e06e68f51f6531520947a53ec7d392f97))

## [0.4.0](https://github.com/rvben/n8nc/compare/v0.3.0...v0.4.0) - 2026-04-02

### Added

- add agent-friendly CLI patterns (schema, auto-JSON, --quiet) ([b2d0c12](https://github.com/rvben/n8nc/commit/b2d0c12583b99474aa3c97b0f26389f62e493eaa))

### Fixed

- extend --quiet coverage and strengthen schema/auto-JSON tests ([c541b95](https://github.com/rvben/n8nc/commit/c541b95838f69807a529a4bf4f55dea2ab877b68))

## [0.3.0](https://github.com/rvben/n8nc/compare/v0.2.1...v0.3.0) - 2026-03-28

### Added

- add search, runs stats, and lint commands ([f587d5e](https://github.com/rvben/n8nc/commit/f587d5e05a3f87c0d8e95d5c6af59f4f23fbcac0))

## [0.2.1](https://github.com/rvben/n8nc/compare/v0.2.0...v0.2.1) - 2026-03-28

### Added

- batch 2 improvements ([f66fcbb](https://github.com/rvben/n8nc/commit/f66fcbb6bcb3601a251899cdbbd8095e6fc4e104))

## [0.2.0](https://github.com/rvben/n8nc/compare/v0.1.0...v0.2.0) - 2026-03-28

### Added

- add archive and unarchive commands ([19856d9](https://github.com/rvben/n8nc/commit/19856d9f97afa39096da6f53cfd0a4d702ace00b))
- **show**: add --tree flag for execution flow visualization ([5c86fa3](https://github.com/rvben/n8nc/commit/5c86fa3c8f41f99a2748f002c1b2f3a0cd69b507))
