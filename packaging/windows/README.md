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
C:\ProgramData\Sentin-NPU\  config.yaml, audit.jsonl
```

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

The service is implemented against the Windows service API inside `sentin-gateway.exe` rather than
by wrapping it in NSSM or WinSW. A wrapper reports its own health rather than the gateway's - the
service control manager would show the wrapper running while the gateway inside it had exited.

Without the installer, the same binary registers itself:

```powershell
sentin-gateway.exe --install-service "C:\ProgramData\Sentin-NPU\config.yaml"
sentin-gateway.exe --uninstall-service
```

## Silent installation

```powershell
sentin-npu-setup-0.0.0.10.exe /VERYSILENT /SUPPRESSMSGBOXES /NORESTART
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
iscc /DVersion=0.0.0.10 `
     /DPayload=..\..\dist\sentin-npu-diag-0.0.0.10-windows-x64 `
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
