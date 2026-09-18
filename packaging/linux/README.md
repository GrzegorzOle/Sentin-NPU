<!--
Copyright 2026 Grzegorz Oleksy
SPDX-License-Identifier: Apache-2.0
-->

# Linux AppImage

One executable file carrying the gateway, the settings console, the OpenVINO runtime, the
quantized model, the diagnostics and the Wazuh integration. It runs on any x86-64 distribution with glibc 2.31 or newer,
and installs nothing: **no Python, no Rust, no OpenVINO**.

```bash
curl -LO https://github.com/GrzegorzOle/Sentin-NPU/releases/latest/download/SHA256SUMS.txt
curl -LO https://github.com/GrzegorzOle/Sentin-NPU/releases/download/v0.0.0.11/Sentin-NPU-0.0.0.11-x86_64.AppImage
sha256sum -c SHA256SUMS.txt --ignore-missing
chmod +x Sentin-NPU-*.AppImage

./Sentin-NPU-*.AppImage --setup     # asks what it needs, writes the configuration
./Sentin-NPU-*.AppImage             # runs the gateway
```

The AppImage filename carries the version; `SHA256SUMS.txt` under `releases/latest/download/` pins
whatever the current release is.

## What `--setup` asks

Enter accepts the value in brackets, so taking every default is a sequence of Enters.

| Question | Default | Why |
|---|---|---|
| Listen port | `4141` | Not 4000: a model router such as LiteLLM commonly holds it. |
| Bind address | `127.0.0.1` | Loopback keeps the inspection point off the network. Containers reach the host by its LAN address, so use `0.0.0.0` if your clients are containers. |
| Upstreams for `/anthropic`, `/openai`, `/google` | the vendors, `localhost:4000` for OpenAI | Callers point at `http://<host>:<port>/<prefix>`. |
| Audit trail | `~/.local/state/sentin-npu/audit.jsonl` | The SIEM trail. Point Wazuh at it - see `packaging/wazuh/`. |
| Inference device | `AUTO` | `AUTO` times every device OpenVINO enumerates and prefers the cheapest one that holds the latency budget. Pin `NPU`, `GPU` or `CPU` to skip the probe. |

The configuration lands in `~/.config/sentin-npu/config.yaml` and is yours to edit afterwards.

**`model_dir` points inside the AppImage**, so it changes when you replace the file with a newer
release. Re-run `--setup` after an upgrade, or edit that one line: a stale path drops the gateway
to layer 1 with only a warning.

## Running it as a service

```bash
./Sentin-NPU-*.AppImage --install-service
systemctl --user status sentin-npu
```

That writes a **user** unit, so no root is involved. Two consequences worth knowing:

- The unit points at the AppImage where it currently sits. Move the file and the service breaks;
  re-run `--install-service` after moving or upgrading it.
- A user service stops when you log out. `sudo loginctl enable-linger $USER` keeps it running,
  which on a workstation is usually what you want - the alternative reads as "it stopped working
  overnight".

`--uninstall-service` reverses it.

## The rest of the commands

```bash
./Sentin-NPU-*.AppImage --console         # the settings window
./Sentin-NPU-*.AppImage --report ~/.local/state/sentin-npu/audit.jsonl report.html
./Sentin-NPU-*.AppImage --docs            # copy the documentation and the Wazuh files out
./Sentin-NPU-*.AppImage --doctor          # what this machine can run the model on, per device
./Sentin-NPU-*.AppImage --bench --m2b-only --device CPU
./Sentin-NPU-*.AppImage --config /etc/sentin-npu/config.yaml
./Sentin-NPU-*.AppImage --help
```

`--console` is the desktop entry's command, so clicking the icon opens the window rather than
starting a gateway in a terminal. It edits `~/.config/sentin-npu/config.yaml` line by line, leaving
comments and anything it does not know about untouched, then restarts the user service so the
change takes effect - the gateway reads its configuration once, at startup, and a console that
saved and stopped there would leave you believing a setting had applied. Each detector is offered
only the verdicts its evidence supports: refusing a request needs a checksum, so an email address
cannot be set to it.

`--report` turns the audit trail into one self-contained HTML file with charts. It is what a SIEM
would answer if there were one. It needs no network, and it cannot disclose an identifier, because
the audit trail has no field that could hold one.

`--doctor` is what to attach to an `npu-report` issue if you have Intel hardware: it compiles and
runs the real model on every device and reports what each one said.

`--docs` exists because documentation inside a squashfs is documentation nobody reads. It copies
the installation guide, the audit event schema, the benchmarks and the whole `wazuh/` directory
into a plain folder you can hand to somebody - and the person who needs the Wazuh rules is usually
not the person holding this file.

## What is inside

```
usr/bin/          sentin-gateway, sentin-ui, sentin-doctor, sentin-bench
usr/lib/          the OpenVINO runtime, with the unversioned soname symlinks dlopen needs
usr/share/sentin-npu/models/seq128, seq512    the quantized IR and its tokenizer
usr/share/sentin-npu/wazuh/                   rules, dashboard, deployment scripts
usr/share/sentin-npu/docs/wazuh/              deploying into Wazuh step by step, EN and PL
usr/share/icons/hicolor/128x128/apps/, .DirIcon    the icon the desktop draws
```

**The icon is the same picture Windows shows**, from 0.4.3 on. Both files come out of
`tools/make_icon.py`, which writes the Linux PNG from the same 128 px frame it packs into the
Windows `.ico` - the same bytes, not a second drawing of the same idea. Until 0.4.3 this was a
placeholder left from the day the AppImage was first built, so the two platforms showed different
pictures of the same program, and the release that gave Windows a real icon walked straight past it.

`AppRun` puts `usr/lib` at the front of `LD_LIBRARY_PATH` before exec'ing the binary. That is the
whole difference between an AppImage that runs anywhere and one that runs only where OpenVINO
happens to be installed: the crate loads the runtime with `dlopen` and looks for **unversioned**
sonames, while the wheel ships only versioned ones.

Device probe results are cached in `~/.cache/sentin-npu`, which is why the first start takes a few
seconds and the second does not. `SENTIN_DEVICE_PROBE=force` re-measures.

## Building it

```bash
./scripts/make-release.sh 0.0.0.11                       # stages the bundle
./packaging/linux/build-appimage.sh \
    dist/sentin-npu-diag-0.0.0.11-linux-x64 0.0.0.11     # packs it
```

`appimagetool` is fetched into `dist/` on first use. CI builds it from the same staged directory
the tarball comes from, so the AppImage and the bundle cannot drift apart, and packs it with
`--appimage-extract-and-run` because runners have no FUSE.

## When it will not start

**`Unable to find the openvino_c library`** means the unversioned symlinks are missing from
`usr/lib`. The build script recreates them; a payload copied through something that drops symlinks
is the usual cause.

**`No configuration at ...`** means `--setup` has not run.

**`layer 2 unavailable; continuing with layer 1`** in the log means the gateway is running but only
the checksum detectors are: the model path is wrong, usually because the AppImage was replaced
without re-running `--setup`. The line to look for on a healthy start is `layer 2 ready device=...`.
