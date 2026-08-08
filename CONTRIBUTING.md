# Contributing to Sentin-NPU

Thanks for your interest in contributing! This is an early-stage proof of
concept, so the most valuable contributions right now are:

- **NPU compatibility reports** — which ops run on your NPU generation, which
  fall back to CPU, driver versions, logs. Open an issue with the `npu-report`
  label.
- **Benchmarks** on hardware we haven't tested (Meteor Lake, Lunar Lake, Arrow Lake).
- **Detection patterns** — regexes/checksums for national identifiers beyond
  PL (PESEL/NIP) — see `sentin/detectors/deterministic/`.
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

<!-- TODO: verify once the code lands -->

```bash
git clone https://github.com/GrzegorzOle/Sentin-NPU.git
cd Sentin-NPU
python -m venv .venv && source .venv/bin/activate
pip install -r requirements-dev.txt
pre-commit install
```

## Pull request checklist

- [ ] Commits are signed off (`git commit -s`)
- [ ] Tests pass: `pytest`
- [ ] New detectors include test cases with both positive and negative samples
- [ ] No sensitive data (real PESEL/IBAN/names) in test fixtures — use
      synthetic values with valid checksums
- [ ] SIEM event schema changes are reflected in `docs/events.md`

## Code style

- Python 3.11+, formatted with `ruff format`, linted with `ruff`
- Type hints required for public functions
- License header (Apache 2.0) in new source files

## Reporting security issues

Do **not** open public issues for vulnerabilities in the gateway itself
(e.g. inspection bypasses). Email: oleksy@cdest.eu

## Questions

Open a GitHub Discussion or an issue with the `question` label.
