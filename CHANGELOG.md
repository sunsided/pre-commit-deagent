# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.1] - 2026-11-19

### Changed

- Rewritten commits keep the original committer timestamp, not only the author timestamp.
- Without signing configured, rewritten commits are explicitly unsigned (`--no-gpg-sign`)
  and the hook says so on stderr.

## [0.1.0] - 2026-11-19

### Added

- `deagent` post-commit hook: strips AI agent `Co-authored-by` trailers, "Generated with"
  lines and session links from `HEAD`, resets author and committer to the configured git
  identity and signs the commit.
- `deagent-check` pre-push / manual hook and `pre-commit-deagent check [<rev-range>]`
  for CI.

[0.1.1]: https://github.com/sunsided/pre-commit-deagent/releases/tag/v0.1.1
[0.1.0]: https://github.com/sunsided/pre-commit-deagent/releases/tag/v0.1.0
