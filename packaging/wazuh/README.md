<!--
Copyright 2026 Grzegorz Oleksy
SPDX-License-Identifier: Apache-2.0
-->

# Sentin-NPU in Wazuh - the files you deploy

This directory holds what gets installed. **The step-by-step guide is elsewhere, and it exists in
two languages** - start there if you are deploying this for the first time:

| | |
|---|---|
| English | `docs/wazuh/deployment-en.md` |
| Polski | `docs/wazuh/wdrozenie-pl.md` |

Where to find them, depending on what you are holding:

| You have | The guides are at |
|---|---|
| a release bundle (`sentin-npu-diag-*`) | `docs/wazuh/` beside this directory |
| the documentation zip (`sentin-npu-docs-*`) | next to this file, in the same folder |
| an installed Windows machine | `C:\Program Files\Sentin-NPU\docs\wazuh\`, and in the Start Menu |
| an AppImage | `--docs <dir>`, then `<dir>/wazuh/` |
| the repository | [`docs/wazuh/`](../../docs/wazuh/) |

They also carry `examples/`: the `<localfile>` blocks for a Windows and a Linux agent, the
centralized `agent.conf` for a group, the gateway's own `audit:` block, fourteen synthetic events to
feed `wazuh-logtest`, and log rotation for both platforms.

**Tested against Wazuh 4.14** (manager, indexer and dashboard on one host, agents on Linux and
Windows). Nothing here is version-specific beyond the saved-object format, which OpenSearch
Dashboards has kept stable since 2.x.

## What is in this directory

| File | What it is | Where it goes |
|---|---|---|
| `sentin_npu_rules.xml` | 18 rules, ids 100500-100531 | the **manager**, `/var/ossec/etc/rules/` |
| `sentin-npu-dashboard.ndjson` | 15 panels plus the dashboard, 16 saved objects | imported in **Dashboards** |
| `deploy-manager.sh` | Installs the rules and the agent group, idempotently, with `--dry-run` | run on the **manager** |
| `build_dashboard.py` | Regenerates the ndjson. Needed only if you renumber the rules or your index pattern is not `wazuh-alerts-*` | anywhere with Python 3 |

The quickest possible path, once the gateway is writing its audit trail and the agent is enrolled:

```bash
sudo ./deploy-manager.sh --dry-run
sudo ./deploy-manager.sh
```

Then import the ndjson in Dashboards. Everything that can go wrong quietly, and everything worth
checking afterwards, is in the guides.

## There is no decoder here, and that is deliberate

The gateway writes one JSON object per line, so Wazuh's own JSON decoder exposes every field as
`data.<name>`. A custom decoder would be one more thing to keep in step with the schema for no gain,
and it would walk straight into the `<decoded_as>` trap that has left rules dead on the manager this
was built against.

The field reference is **`docs/events.md`**, which is authoritative: any change to the schema updates
it in the same commit. It sits beside this directory in a release bundle (`../docs/events.md`), in
the repository at
[`docs/events.md`](https://github.com/GrzegorzOle/Sentin-NPU/blob/main/docs/events.md), and inside
the AppImage under `usr/share/sentin-npu/docs/` - extract it with `--docs`.

## The dashboard is generated, not hand-written

Saved-object JSON is unreviewable by eye: a panel change should read as a two-line diff in
`build_dashboard.py` rather than as a reshuffled 22 KB blob. Four panels filter by rule id, which is
why those ids are constants at the top of the generator - a site that has to renumber the rules
changes them in one place and regenerates:

```bash
python3 build_dashboard.py --index-pattern 'your-pattern-id'
```
