<!--
Copyright 2026 Grzegorz Oleksy
SPDX-License-Identifier: Apache-2.0
-->

# Sentin-NPU in Wazuh

Everything needed to get the gateway's audit trail into Wazuh and onto a dashboard: rules, an
agent configuration snippet, and a generated set of saved objects. Written for a Wazuh
administrator who has never seen this project - you should not need to read any Rust to deploy it.

**Tested against Wazuh 4.14** (manager, indexer and dashboard on one host, agents on Linux and
Windows). Nothing here is version-specific beyond the saved-object format, which OpenSearch
Dashboards has kept stable since 2.x.

| File | What it is |
|---|---|
| `sentin_npu_rules.xml` | 18 rules, ids 100500-100531. Install on the **manager**. |
| `agent-localfile.conf` | The `<localfile>` block that ships events. Install on the **agent**, or push it to a group. |
| `sentin-npu-dashboard.ndjson` | 16 panels plus the dashboard. Import in **Dashboards**. |
| `build_dashboard.py` | Regenerates the ndjson. Only needed if you change panels or index pattern. |

**There is no decoder in this directory, and that is deliberate.** The gateway writes one JSON
object per line, so Wazuh's own JSON decoder exposes every field as `data.<name>`. A custom decoder
would be one more thing to keep in step with the schema for no gain.

---

## 1. Make the gateway emit

In the gateway's `config.yaml`:

```yaml
audit:
  jsonl:
    enabled: true
    path: /var/log/sentin-npu/audit.jsonl     # absolute, always
```

**Use an absolute path.** A relative one resolves against the gateway's working directory, so the
file appears somewhere nobody is watching and the integration collects nothing while looking
configured. The gateway must be able to create and append to it; the Wazuh agent must be able to
read it.

The gateway also speaks **CEF over syslog** if you would rather not read a file - see the end of
this document. JSON over a file is recommended: it is the only route where no field is lost to
formatting.

## 2. Collect it

**Option A, one agent.** Paste the matching block from `agent-localfile.conf` into that agent's
`ossec.conf` inside `<ossec_config>`, adjust the path, and restart the agent:

```bash
sudo systemctl restart wazuh-agent          # Linux
Restart-Service WazuhSvc                    # Windows, elevated
```

**Option B, centrally, and the one to prefer for more than a handful of machines.** Push it from
the manager as a group configuration. This also works where you cannot edit the agent's
`ossec.conf` at all, which on Windows means anywhere without local administrator rights:

```bash
sudo /var/ossec/bin/agent_groups -a -g sentin-npu -q
sudo /var/ossec/bin/agent_groups -a -i <AGENT_ID> -g sentin-npu -q

# Put ONLY the <agent_config> form in the group file - not the <ossec_config> wrapper:
sudo tee /var/ossec/etc/shared/sentin-npu/agent.conf >/dev/null <<'EOF'
<agent_config>
  <localfile>
    <location>/var/log/sentin-npu/audit.jsonl</location>
    <log_format>json</log_format>
  </localfile>
</agent_config>
EOF
sudo chown wazuh:wazuh /var/ossec/etc/shared/sentin-npu/agent.conf
sudo chmod 660 /var/ossec/etc/shared/sentin-npu/agent.conf
sudo /var/ossec/bin/verify-agent-conf
sudo /var/ossec/bin/agent_control -R -u <AGENT_ID>      # push now instead of waiting
```

That last line is not an optimisation, it is the step. The agent downloads the new shared
configuration within a minute, and then goes on following the *old* file until it restarts -
reporting zero events and zero drops, which looks exactly like a quiet system.

**Do not leave a backup beside it in the group directory.** The manager merges every file in
`/var/ossec/etc/shared/<group>/` into `merged.mg`, so an `agent.conf.bak` is not a backup: it is a
second live configuration, and the agent will collect both paths. Keep backups outside the group.

A group is worth the extra step even for one agent: it keeps the change in one reviewable place,
it does not touch the other agents, and removing the group removes the integration.

**Check the agent is actually reading the file** before going further. On the agent:

```bash
grep -A3 audit.jsonl /var/ossec/var/run/wazuh-logcollector.state    # Linux
type "C:\Program Files (x86)\ossec-agent\wazuh-logcollector.state"  # Windows
```

The file must appear with a rising `events` count. On Windows that state file is readable without
elevation even when `ossec.conf` is not, which makes it the fastest way to tell "not collected"
from "collected but no rule matched".

## 3. Install the rules

```bash
sudo install -o wazuh -g wazuh -m 660 sentin_npu_rules.xml /var/ossec/etc/rules/
sudo /var/ossec/bin/wazuh-analysisd -t          # must exit 0 before you restart anything
sudo systemctl restart wazuh-manager
```

`wazuh-analysisd -t` parses the whole ruleset without touching the running service. On a busy
manager, run it and read it: a syntax error found here costs a second, and found after a restart
costs however long it takes someone to notice alerts stopped.

**If ids 100500-100531 are taken on your manager**, renumber this file and change the rule-id
constants at the top of `build_dashboard.py`, then regenerate the ndjson. Five panels filter by
rule id and will be empty otherwise.

**Every event kind must appear in rule 100500's `event` field.** It is the parent the others hang
off, and a child whose parent never matches never fires - silently. That is not hypothetical:
`attachment_skipped` was added to the gateway and not to that line, and rule 100524 was dead until
somebody counted the alerts and found none.

Validate against a real line, which is the step that catches a schema drift:

```bash
sudo /var/ossec/bin/wazuh-logtest
# paste one line from audit.jsonl, then look for:
#   Phase 2: decoder 'json'
#   Phase 3: rule '100502' level 7
```

## 4. Import the dashboard

Dashboards -> **Stack Management** -> **Saved Objects** -> **Import** ->
`sentin-npu-dashboard.ndjson` -> *Automatically overwrite conflicts*.

The panels reference the alerts index pattern by the id `wazuh-alerts-*`, which is what a stock
Wazuh install uses. If yours differs, regenerate rather than fixing sixteen panels by hand:

```bash
python3 build_dashboard.py --index-pattern 'your-pattern-id'
```

Open **Sentin-NPU - data leaving for LLMs**. The default time range is whatever your Dashboards
default is; the panels do not pin one, so a dashboard showing nothing is usually a time range
rather than a broken import.

---

## What the panels answer

| Panel | Question |
|---|---|
| Blocked requests | Did anyone get refused? These are the users who will ask why. |
| High-value identifiers stopped | PESEL, IBAN, payment card. Individually reportable, unlike a name. |
| Inspection gaps | Did anything go out **uninspected**? Should be zero. |
| Repeat offenders | One paste is an accident, eight in five minutes is a habit. |
| Detections over time, by decision | The shape: flat is normal, a step is a new integration, a ramp is a habit forming. |
| What was detected | By data type, so layer-1 and layer-2 findings sit in one picture. |
| Which workstation | `client_addr`. What turns a detection into something actionable. |
| Which model the data was heading for | `upstream_model`. A local model and a hosted one abroad are very different findings. |
| What the gateway did | Blocked / masked / advised / observed. A wall of blocks means the policy is miscalibrated. |
| Through which provider | The adapter: anthropic, openai, google. |
| Who sent what, where | The table an analyst copies from. |
| Which files keep coming back | One row per document, keyed by the digest of its bytes. The same value under two workstations, or again after a block, is an abuse path rather than an accident. |
| Typed, or attached | `source`. One person's slip against a file full of other people's data. |
| What kind of file | PDF, Office, text - or opaque, meaning it could not be read at all. |
| High-value identifiers inside documents | PESEL, IBAN or a card inside an attachment, which cannot be masked. |
| Where inspection ran | Which device executed the model, and which model version. |

## Fields you can query

All under `data.`, straight from the JSON. The schema is authoritative in **`docs/events.md`** and
any change to it updates that file in the same commit. That file sits next to this one in a release
bundle (`../docs/events.md`), in the repository at
[`docs/events.md`](https://github.com/GrzegorzOle/Sentin-NPU/blob/main/docs/events.md), and inside
the AppImage under `usr/share/sentin-npu/docs/` - extract it with `--docs`.

| Field | Example | Note |
|---|---|---|
| `data.event` | `pii_detected` | also `decision_made`, `inspection_timeout`, `attachment_skipped`, `device_fallback`, `gateway_start`, `gateway_stop` |
| `data.detector` | `pesel` | the configured detector key |
| `data.data_type` | `PESEL` | `NIP`, `REGON`, `IBAN`, `PAYMENT_CARD`, `EMAIL`, `PHONE_PL`, `PERSON`, `ORGANIZATION`, `LOCATION` |
| `data.decision` | `masked` | `observed`, `advised`, `masked`, `blocked`, `user_override` |
| `data.client_addr` | `10.1.2.3` | who sent it. No port: it changes per request, and a frequency rule keyed on it would never group |
| `data.upstream_model` | `claude-sonnet-4` | the model the data was heading for |
| `data.provider` | `openai` | the adapter that handled it |
| `data.target_host` | `api.anthropic.com` | host only, never a full URL |
| `data.content_sha256` | `sha256:...` | correlates the events of one request |
| `data.model_id` | `seq128` | the **inspecting** NER model, not the one above |
| `data.device` | `NPU` | which device executed inspection |
| `data.source` | `attachment` | `prompt` or `attachment` - what a typed identifier and an attached document are told apart by |
| `data.attachment_kind` | `pdf` | `pdf`, `ooxml`, `text`, `opaque`; `opaque` means it could not be read |
| `data.attachment_bytes` | `664` | decoded size |

**`model_id` and `upstream_model` are different models.** It is the pair most likely to be
confused: group "where is our data going" by `upstream_model`, and "which detector version said
so" by `model_id`.

**Events never contain the detected text.** There is no field capable of holding it; where the
content matters for correlation, `content_sha256` covers the whole inspected payload rather than
the identifier, so it cannot be brute-forced back to an eleven-digit number. This is what makes it
safe to point a DLP trail at a SOC that many people can read.

**`client_addr` is personal data** in most deployments, exactly as any proxy log is. It is recorded
because a decision without an owner cannot be acted on. If your retention rules forbid it, turn the
sink off rather than filtering the field: the same value reaches every emitter.

## Troubleshooting

**Nothing in the dashboard.** In this order:

1. Time range. It is the answer more often than everything below combined.
2. Is the agent reading the file? `wazuh-logcollector.state` on the agent, as above.
3. Are alerts being written? `grep sentin /var/ossec/logs/alerts/alerts.json`.
4. Is the raw event arriving but matching nothing? **A log that matches no rule is dropped
   silently** - no alert, no archive unless `logall_json` is on. That is what an empty dashboard
   with a healthy collector means. `wazuh-logtest` tells you in seconds.
5. Index pattern id in the import. A panel with a broken reference renders empty rather than
   erroring.

**Alerts arrive at level 3 with a generic description.** Something else matched first, or the
parent rule 100500 did not. Check that the JSON decoder ran (`Phase 2: decoder 'json'` in
`wazuh-logtest`) - if the line was collected as `syslog` rather than `json`, the fields are not
there to match on and `log_format` in the localfile block is wrong.

**Rules do not fire after editing.** The manager must be restarted; `wazuh-analysisd -t` only
validates. And `<if_sid>` resolves in file order, so a child defined before its parent is dead.

**Everything works, then stops after a rotation.** The collector follows a truncation. If your
rotation renames and recreates, events written between the two are lost; prefer `copytruncate`.

## Removing it

```bash
sudo rm /var/ossec/etc/rules/sentin_npu_rules.xml
sudo /var/ossec/bin/agent_groups -r -i <AGENT_ID> -g sentin-npu -q
sudo systemctl restart wazuh-manager
```

Then delete the saved objects in Dashboards (they are all prefixed `sentin-npu-`).

## The CEF alternative

If you would rather not read a file, the gateway also emits CEF over syslog:

```yaml
audit:
  syslog_cef:
    enabled: true
    address: 10.0.0.5:514
    protocol: udp
```

Point it at the manager's syslog listener. Fields land as CEF extensions: `src`/`spt` for the
caller, `cs5` for the upstream model, `cs6` for the provider, `cs1` detector, `cs2` data type,
`cs3` model id, `cs4` device, `act` decision, `dhost` upstream host, `fileHash` content digest.
The rules in this directory match the JSON fields and **will not fire on CEF** - you would need a
decoder for it. That is the honest reason the file route is recommended.

OTLP is also available and is out of scope here: Wazuh has no OTLP receiver.
