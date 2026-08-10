# Contributing to Sentin-NPU

Thanks for your interest in contributing! This is an early-stage proof of
concept, so the most valuable contributions right now are:

- **NPU compatibility reports** - which ops run on your NPU generation, which
  fall back to CPU, driver versions, logs. Open an issue with the `npu-report`
  label.
- **Benchmarks** on hardware we haven't tested (Meteor Lake, Lunar Lake, Arrow Lake).
- **Detection patterns** - checksums for national identifiers beyond PL
  (PESEL/NIP/REGON) - see `gateway/crates/sentin-detect/`.
- Bug fixes and documentation.

Before starting larger work (new detection layer, new provider adapter),
please open an issue first so we can align on design.

## Developer Certificate of Origin (DCO)

All contributions require a DCO sign-off. By signing off, you certify that you
wrote the contribution or otherwise have the right to submit it under the
Apache 2.0 license (see https://developercertificate.org/).

Sign off each commit:

```bash
git commit -s -m "Add IBAN checksum validation"
```

This adds a line to the commit message:

```
Signed-off-by: Your Name <your.email@example.com>
```

Pull requests with unsigned commits will fail the DCO check and cannot be
merged. To fix a missing sign-off on your last commit:

```bash
git commit --amend -s --no-edit
```

## Development setup

The gateway is Rust; Python is only the offline model toolchain. Most contributions need one or
the other, not both.

```bash
git clone https://github.com/GrzegorzOle/Sentin-NPU.git
cd Sentin-NPU

# Rust side (the gateway that ships) - needs a stable toolchain, MSRV 1.82.
# The Cargo workspace is in gateway/, not at the repo root.
cd gateway && cargo test --workspace

# Python side (model toolchain), only if you are touching tools/
python3.11 -m venv tools/.venv
tools/.venv/bin/pip install -r tools/requirements-dev.txt
tools/.venv/bin/pre-commit install
```

`requirements.txt` holds human-facing minimum versions; `requirements.lock.txt` is the resolved
set that CI installs. Bump the lock deliberately and re-run `tools/validate_model.py`.

## Pull request checklist

- [ ] Commits are signed off (`git commit -s`)
- [ ] Rust (from `gateway/`): `cargo test --workspace`, `cargo fmt --all --check`,
      `cargo clippy --all-targets -- -D warnings`
- [ ] Python (only if `tools/` changed): `tools/.venv/bin/ruff check tools/` and `ruff format --check tools/`
- [ ] New detectors include test cases with both positive and negative samples
- [ ] No sensitive data (real PESEL/IBAN/names) in test fixtures - use
      synthetic values with valid checksums. `sentin_detect::testdata` generates
      them with correct check digits; an identifier invented by eye fails its own
      checksum and the resulting test failure blames the code, not the fixture
- [ ] SIEM event schema changes are reflected in `docs/events.md`
- [ ] Benchmark numbers come with date, commit, hardware and versions, or they are not results

## Code style

- Rust: edition 2021, MSRV 1.82, `rustfmt`, `clippy -D warnings` (CI enforces all three)
- Python 3.11+, formatted with `ruff format`, linted with `ruff`
- Type hints required for public functions
- License header (Apache 2.0) in new source files
- Do not trust training data or memory for OpenVINO APIs - the library moves fast; verify against
  the version pinned in `tools/requirements.lock.txt`

## Reporting security issues

Do **not** open public issues for vulnerabilities in the gateway itself
(e.g. inspection bypasses). Email: oleksy@cdest.eu

## Questions

Open a GitHub Discussion or an issue with the `question` label.
