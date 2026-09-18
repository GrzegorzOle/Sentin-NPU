<!--
Copyright 2026 Grzegorz Oleksy
SPDX-License-Identifier: Apache-2.0
-->

# Sentin-NPU and Wazuh - deployment documentation

# Sentin-NPU i Wazuh - dokumentacja wdrożenia

**EN** - everything a Wazuh administrator needs to get the gateway's audit trail into Wazuh and onto
a dashboard, with ready-to-paste example files. Written for somebody who has never seen this project
and does not intend to read its source.

**PL** - wszystko, czego administrator Wazuha potrzebuje, żeby wprowadzić ślad audytowy bramy do
Wazuha i na dashboard, razem z gotowymi do wklejenia plikami przykładowymi. Napisane dla kogoś, kto
nigdy nie widział tego projektu i nie zamierza czytać jego kodu.

| | |
|---|---|
| **English** | **[deployment-en.md](deployment-en.md)** |
| **Polski** | **[wdrozenie-pl.md](wdrozenie-pl.md)** |

Both guides cover the same ground and are kept in step: the transport decision, the gateway's own
configuration, collection on the agent, the rules on the manager, the dashboard, end-to-end
verification, troubleshooting and removal.

Oba przewodniki opisują to samo i są utrzymywane równolegle: wybór transportu, konfiguracja samej
bramy, zbieranie po stronie agenta, reguły na managerze, dashboard, weryfikacja end to end,
diagnostyka i usuwanie.

## Example files / Pliki przykładowe

In [`examples/`](examples/). Comments inside each file are bilingual, so the file is usable without
the guide beside it.

W katalogu [`examples/`](examples/). Komentarze w każdym pliku są dwujęzyczne, więc plik da się
wykorzystać bez przewodnika pod ręką.

| File / Plik | EN | PL |
|---|---|---|
| [`localfile-windows.conf`](examples/localfile-windows.conf) | `<localfile>` block for one Windows agent | blok `<localfile>` dla jednego agenta na Windows |
| [`localfile-linux.conf`](examples/localfile-linux.conf) | the same for Linux | to samo dla Linuksa |
| [`agent-group.conf`](examples/agent-group.conf) | centralized `agent.conf` for a group, both platforms | centralny `agent.conf` dla grupy, obie platformy |
| [`gateway-audit.yaml`](examples/gateway-audit.yaml) | the gateway's `audit:` block | blok `audit:` bramy |
| [`sample-audit.jsonl`](examples/sample-audit.jsonl) | 14 synthetic events, one per rule, for `wazuh-logtest` | 14 syntetycznych zdarzeń, po jednym na regułę, do `wazuh-logtest` |
| [`rotate-audit.ps1`](examples/rotate-audit.ps1) | audit trail rotation on Windows | rotacja śladu audytowego na Windows |
| [`logrotate-sentin-npu.conf`](examples/logrotate-sentin-npu.conf) | the same for Linux | to samo dla Linuksa |

**EN** - the events in `sample-audit.jsonl` are synthetic and carry no personal data of any kind.
They could not: the schema has no field capable of holding detected text, which is the property that
makes this trail safe to point at a SOC.

**PL** - zdarzenia w `sample-audit.jsonl` są syntetyczne i nie niosą żadnych danych osobowych. Nie
mogłyby: schemat nie ma pola zdolnego pomieścić wykryty tekst, i to właśnie ta własność pozwala
skierować ten ślad do SOC-a.

## What you deploy is next door / Pliki do wdrożenia są obok

**EN** - the rules, the dashboard and the deployment script are **not** in this directory. They sit
in `wazuh/`, one level up from `docs/`: `sentin_npu_rules.xml`, `sentin-npu-dashboard.ndjson`,
`deploy-manager.sh` and `build_dashboard.py`. In the repository that directory is
[`packaging/wazuh/`](../../packaging/wazuh/); on an installed Windows machine it is
`C:\Program Files\Sentin-NPU\wazuh\`.

**PL** - reguł, dashboardu ani skryptu wdrożeniowego **nie ma** w tym katalogu. Leżą w `wazuh/`,
poziom wyżej niż `docs/`: `sentin_npu_rules.xml`, `sentin-npu-dashboard.ndjson`, `deploy-manager.sh`
i `build_dashboard.py`. W repozytorium jest to [`packaging/wazuh/`](../../packaging/wazuh/), a na
zainstalowanej maszynie z Windows `C:\Program Files\Sentin-NPU\wazuh\`.

## The field reference / Opis pól

[`../events.md`](../events.md). **EN** - authoritative and binding: any change to the schema updates
that file in the same commit, so a decoder or a rule written against it cannot silently fall behind.
**PL** - rozstrzygający i wiążący: każda zmiana schematu aktualizuje ten plik w tym samym commicie,
więc dekoder ani reguła napisane na jego podstawie nie zostaną po cichu w tyle.
