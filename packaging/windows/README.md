<!--
Copyright 2026 Grzegorz Oleksy
SPDX-License-Identifier: Apache-2.0
-->

# Windows installer

One `.exe` that carries the gateway, the OpenVINO runtime, the quantized model, the diagnostics and
the Wazuh integration. Nothing is downloaded during installation and nothing else has to be on the
machine: no Rust, no Python, no OpenVINO.

Download `sentin-npu-setup-<version>.exe` from the
[latest release](https://github.com/GrzegorzOle/Sentin-NPU/releases/latest), check it against
`SHA256SUMS.txt`, and run it. It needs administrator rights, because it installs a service.

## What the wizard asks

Only the questions whose wrong answers are silent:

| Question | Default | Why it is asked |
|---|---|---|
| Listen port | `4141` | Not 4000: a model router such as LiteLLM commonly holds that port, and a gateway that cannot bind is a poor first impression. |
| Bind address | `127.0.0.1` | Loopback keeps the inspection point off the network. Use `0.0.0.0` if clients are containers or other machines - they cannot reach loopback on the host. |
| Audit file | `C:\ProgramData\Sentin-NPU\audit.jsonl` | Where the SIEM trail is written. Outside Program Files, so an uninstall does not take the history with it. |
| Upstreams for `/anthropic`, `/openai`, `/google` | the vendors, and `localhost:4000` for OpenAI | Callers point at `http://<host>:<port>/<prefix>`. Change the OpenAI one if you route through a local router. |
| Install as a service, start now | both yes | A gateway started by hand is a gateway that is sometimes not running, and the traffic it misses is exactly the traffic nobody inspected. |

Answers become `C:\ProgramData\Sentin-NPU\config.yaml`. **An existing configuration is never
overwritten**: an upgrade writes `config.yaml.new` beside it and says so, because the detector
policy a site has tuned is theirs, not the installer's.

`model_dir` is written as an **absolute** path. A relative one resolves against the service's
working directory, and layer 2 would go missing with nothing but a warning in the log - the trap
this project has hit more often than any other.

## What lands where

```
C:\Program Files\Sentin-NPU\        sentin-gateway.exe, sentin-doctor.exe, sentin-bench.exe
                            lib\    OpenVINO runtime
                            models\ the quantized IR and its tokenizer
                            wazuh\  rules, dashboard, deployment guide
                            docs\   installation, the audit schema, benchmarks, licences
C:\ProgramData\Sentin-NPU\  config.yaml, audit.jsonl, sentin-gateway.log
```

The OpenVINO runtime sits in `lib\` and nothing is added to the machine's `PATH`. The binaries put
that directory on their own search path at startup, which is what makes the installation work
without changing anything outside itself. It is worth knowing because the first release of this
installer did *not* do it: Windows searches the executable's own directory and `PATH`, neither of
which is `lib\`, so the service ran with layer 2 missing and nothing said so.

The Start Menu group links the installation guide, the audit event schema and the Wazuh deployment
guide, so nobody has to know they are on disk. The same material is published on its own as
`sentin-npu-docs-<version>.zip`, which is what to send to whoever runs your SIEM.

## The service

```powershell
sc query   SentinNPU     # is it running
sc stop    SentinNPU
sc start   SentinNPU
sc qc      SentinNPU     # which configuration it uses - it is in the binPath
```

It runs as LocalSystem, starts automatically at boot, and **stops gracefully**: on stop it
stops accepting connections and lets in-flight requests finish, because the traffic passing through
is somebody's real work.

### Checking that it inspects, not merely that it runs

A service has no console, so the gateway writes to
`C:\ProgramData\Sentin-NPU\sentin-gateway.log` (Start Menu: *Service log*). One line there decides
whether the installation is doing its job:

```
layer 2 ready device=CPU ... selection="device=CPU objective=Cost ceiling=80ms [CPU=8.9ms]"
layer 2 unavailable; continuing with layer 1
```

The second is not an error and does not stop anything - the gateway is in front of somebody's real
work, so it degrades rather than refuses. Checksum detection (PESEL, NIP, REGON, IBAN, payment
cards) keeps working; named entity detection does not, so people, organisations and places go
unnoticed. **A gateway inspecting less than it claims looks exactly like a working gateway**, which
is why the installer reads this file itself after starting the service and says which of the two it
found.

The log is rotated once at start when it passes 8 MB, to `sentin-gateway.log.1`.

The other end of the same check is the audit trail: an event carrying `device` and a `person` or
`location` detector could only have come from layer 2.

The service is implemented against the Windows service API inside `sentin-gateway.exe` rather than
by wrapping it in NSSM or WinSW. A wrapper reports its own health rather than the gateway's - the
service control manager would show the wrapper running while the gateway inside it had exited.

Without the installer, the same binary registers itself:

```powershell
sentin-gateway.exe --install-service "C:\ProgramData\Sentin-NPU\config.yaml"
sentin-gateway.exe --uninstall-service
```

## Upgrading over an existing installation

Run the new installer. It does not need the old one uninstalled, and it takes care of the two
things that would otherwise go wrong:

1. **The running service is stopped first.** It holds `sentin-gateway.exe` open, and replacing a
   file in use fails - the difference between an upgrade and a support call. The service is started
   again at the end, unless you clear "start it when this installer finishes".
2. **The service registration is updated, not recreated.** Creating a service that already exists
   fails, and until 0.1.1 the installer reported that as "the service could not be registered" and
   stopped - *after* it had stopped the old service to replace its files. The result was new
   binaries, an error box, and a gateway that was not running. Updating also keeps anything you
   changed about the service, such as its recovery actions, instead of resetting it every upgrade.

**Your settings are kept.** An existing `C:\ProgramData\Sentin-NPU\config.yaml` is never
overwritten: the wizard's answers are written to `config.yaml.new` beside it and the installer says
so. The service keeps running the configuration you already had, so a detector policy you tuned
survives an upgrade untouched - and if you want the new defaults, the file to compare against is
right there. The audit trail and the log are left alone for the same reason.

What an upgrade *does* replace is everything under `C:\Program Files\Sentin-NPU\`: the binaries, the
OpenVINO runtime, the model, the Wazuh files and the documentation.

## Silent installation

```powershell
sentin-npu-setup-0.2.0.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
```

Silent mode takes every default, including installing and starting the service. To deploy a
prepared configuration, put your `config.yaml` in `C:\ProgramData\Sentin-NPU\` **before** running
the installer: it will be kept, and your answers will not be needed.

`/DIR="D:\Sentin-NPU"` changes the program directory. `/COMPONENTS="gateway"` installs the gateway
alone, without the diagnostics or the Wazuh files.

## Building it

Needs a Windows machine with [Inno Setup 6](https://jrsoftware.org/isinfo.php) and a staged bundle
(what `scripts/make-release.sh` produces):

```powershell
iscc /DVersion=0.2.0 `
     /DPayload=..\..\dist\sentin-npu-diag-0.2.0-windows-x64 `
     sentin-npu.iss
```

CI does this on every tag, from the bundle already published for that release, so the installer and
the zip carry byte-identical binaries. The script is also compiled against a stand-in payload on
every push to `packaging/` (`.github/workflows/packaging.yml`), because a typo found during a
release is found at the worst possible moment.

## Uninstalling

Through Settings, or `unins000.exe` in the program directory. The service is stopped and removed
first. `C:\ProgramData\Sentin-NPU\` is left behind on purpose: it holds the configuration and the
audit trail, and an uninstaller that deletes an audit trail is an uninstaller nobody should trust.
