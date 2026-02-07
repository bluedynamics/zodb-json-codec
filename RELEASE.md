# Release Process

## Version Management

The package version is defined in `Cargo.toml` as the single source of truth.
`pyproject.toml` uses `dynamic = ["version"]` to read from it automatically.

## Prerequisites

### PyPI Trusted Publishing

Both Test PyPI and real PyPI use OIDC trusted publishing (no API tokens needed).
Set this up once per project:

1. **Test PyPI**: Go to https://test.pypi.org/manage/project/zodb-json-codec/settings/publishing/
   - Add a GitHub publisher: owner=`bluedynamics`, repo=`zodb-json-codec`,
     workflow=`release.yml`, environment=`release-test-pypi`

2. **PyPI**: Go to https://pypi.org/manage/project/zodb-json-codec/settings/publishing/
   - Add a GitHub publisher: owner=`bluedynamics`, repo=`zodb-json-codec`,
     workflow=`release.yml`, environment=`release-pypi`

3. **GitHub Environments**: In the repo settings, create two environments:
   - `release-test-pypi`
   - `release-pypi` (optionally add required reviewers for extra safety)

### cargo-release (recommended)

Install for streamlined releases:

```bash
cargo install cargo-release
```

## Making a Release

### 1. Bump the version

Edit `Cargo.toml` and update the version:

```toml
[package]
version = "0.2.0"  # was 0.1.0
```

Or use `cargo-release` to do it automatically:

```bash
cargo release version patch  # 0.1.0 -> 0.1.1
cargo release version minor  # 0.1.0 -> 0.2.0
cargo release version major  # 0.1.0 -> 1.0.0
```

### 2. Commit and push

```bash
git add Cargo.toml
git commit -m "Bump version to 0.2.0"
git push
```

This triggers the release workflow on `main`, which builds all wheels and
publishes to **Test PyPI** (dev version).

### 3. Create a GitHub Release

1. Go to https://github.com/bluedynamics/zodb-json-codec/releases/new
2. Create a new tag: `v0.2.0` (matching the version in `Cargo.toml`)
3. Set the release title: `v0.2.0`
4. Add release notes (or use "Generate release notes")
5. Click "Publish release"

This triggers the release workflow again, building all wheels and publishing
to **PyPI**.

## What the Release Workflow Builds

| Platform | Architecture | Wheels |
|----------|-------------|--------|
| Linux (manylinux) | x86_64 | Python 3.10-3.13 |
| Linux (manylinux) | aarch64 | Python 3.10-3.13 |
| macOS | x86_64 | Python 3.10-3.13 |
| macOS | arm64 (Apple Silicon) | Python 3.10-3.13 |
| Windows | x64 | Python 3.10-3.13 |
| Source | - | sdist |

## Workflow Overview

```
push to main  ──> CI (tests + perf check)
                   └──> Build wheels (5 platforms)
                         └──> Publish to Test PyPI

GitHub Release ──> CI (tests + perf check)
                   └──> Build wheels (5 platforms)
                         └──> Publish to PyPI
```

## Adding Python Version Support

When a new Python version is supported by PyO3:

1. Update `PYTHON_TARGETS` in `.github/workflows/release.yml`
2. Add the version to the test matrix in `.github/workflows/ci.yml`
3. Test locally with `maturin develop` on the new Python version

## Troubleshooting

### PyO3 doesn't support Python 3.X yet

If `--find-interpreter` picks up a Python version newer than PyO3 supports,
the build will fail. The workflow uses explicit `-i 3.10 -i 3.11 ...` flags
to avoid this. Update `PYTHON_TARGETS` when PyO3 adds support.

### Trusted publishing fails

Verify that:
- The GitHub environment name matches exactly (`release-test-pypi` / `release-pypi`)
- The workflow filename matches what's registered on PyPI (`release.yml`)
- The `id-token: write` permission is set on the publish job
