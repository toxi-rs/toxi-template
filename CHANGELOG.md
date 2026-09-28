# Changelog — `toxi-template`

Per-crate history extracted from the monolith changelog
([meshackbahati/toxi](https://github.com/meshackbahati/toxi/blob/main/CHANGELOG.md)),
which remains the full documentation hub.

## Unreleased

- **toxi-template** (`3.1.2`): renderer threads one output buffer
  through all nesting levels; static files serve small files from a
  size-capped mtime-validated memory cache with disk fallback.
  `StaticFiles::with_max_cached_file_size` tunes the cap.

## Unreleased

- **toxi-template** (`3.1.1`): loop rendering reuses one context instead
  of cloning the full context map per element.

## 3.1.5

- **toxi-template** (`3.1.1`): `TemplateContext` caches the compiled engine
  after a single directory load instead of re-traversing and re-parsing on
  every render. Added `reload()` to pick up disk changes without restart.
