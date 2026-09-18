<!--
Copyright 2026 Grzegorz Oleksy
SPDX-License-Identifier: Apache-2.0
-->

# Deploying Sentin-NPU into Wazuh

Step by step, for a Wazuh administrator. You should not need to read any Rust, or know anything
about the gateway beyond where it writes its audit trail.

**Polish version: [`wdrozenie-pl.md`](wdrozenie-pl.md).**

## What this guide assumes

| Assumption | If it does not hold |
|---|---|
| A Wazuh manager is running and you can reach it over SSH with `sudo` | Install it first; nothing here is specific to how it was installed |
| The machine running the gateway is already an **enrolled agent** and shows as `Active` | Enrol it the usual way (`agent-auth` or the deployment command from the dashboard); this guide starts after that |
| Wazuh **4.x**, tested on 4.14 | The rules use nothing newer than 4.0; only the saved-object format is version-sensitive, and it has been stable since OpenSearch Dashboards 2.x |
| The gateway is installed and inspecting traffic | See `docs/install-windows.md` or `docs/install-linux.md` in this same archive |

Deployment takes about twenty minutes, and roughly fifteen of them are the dashboard import.

## Where the files are

Two directories, and the split is deliberate: **what you deploy** sits in `wazuh/`, **what you
read** sits here in `docs/wazuh/`.

| | Deployables (`wazuh/`) | Documentation (`docs/wazuh/`) |
|---|---|---|
| In a release bundle | `wazuh/` | `docs/wazuh/` |
| Installed on Windows | `C:\Program Files\Sentin-NPU\wazuh\` | `C:\Program Files\Sentin-NPU\docs\wazuh\` |
| From the AppImage | `--docs <dir>` writes both into `<dir>/wazuh/` and `<dir>/wazuh/docs` stays beside it | same |
| In the repository | `packaging/wazuh/` | `docs/wazuh/` |

What is in each:

| File | What it is |
|---|---|
| `wazuh/sentin_npu_rules.xml` | 18 rules, ids 100500-100531. Install on the **manager**. |
| `wazuh/sentin-npu-dashboard.ndjson` | 15 panels plus the dashboard. Import in **Dashboards**. |
| `wazuh/deploy-manager.sh` | Does sections 3 and 4 for you, idempotently, with a dry run. |
| `wazuh/build_dashboard.py` | Regenerates the ndjson. Only needed if you renumber rules or change the index pattern. |
| `docs/wazuh/examples/localfile-windows.conf` | The `<localfile>` block for a single Windows agent. |
| `docs/wazuh/examples/localfile-linux.conf` | The same for Linux. |
| `docs/wazuh/examples/agent-group.conf` | The `<agent_config>` form, pushed from the manager to a group. Both platforms in one file. |
| `docs/wazuh/examples/gateway-audit.yaml` | The `audit:` block of the gateway's own configuration. |
| `docs/wazuh/examples/sample-audit.jsonl` | Fourteen synthetic events covering every rule. Feed these to `wazuh-logtest` before you have real traffic. |
| `docs/wazuh/examples/rotate-audit.ps1` | Rotation on Windows. The gateway never rotates its own trail. |
| `docs/wazuh/examples/logrotate-sentin-npu.conf` | The same for Linux. |

---

## 1. Choose the transport

This is the "protocol" decision, and there is a recommended answer.

| | **JSONL file + `localfile`** (recommended) | CEF over syslog | OTLP |
|---|---|---|---|
| What the gateway does | Appends one JSON object per line to a file | Sends a CEF line to a syslog listener, UDP or TCP | POSTs OTLP over HTTP with JSON encoding |
| How Wazuh receives it | The agent's log collector reads the file and the built-in `json` decoder exposes every field as `data.<name>` | The manager's syslog listener, if you enable one | Not at all |
| Do the shipped rules fire? | **Yes** | **No** - they match JSON field names. You would have to write a decoder | No |
| Survives the gateway being unreachable? | Yes, the file is local | No, UDP silently drops | No |
| Needs a new listener open | No | Yes | Yes |

Take the file route unless something makes it impossible. It is the only one where no field is lost
to formatting, the agent is already running on that machine, and there is **no decoder to write or
maintain** - the gateway writes JSON, and Wazuh's own JSON decoder does the parsing.

The CEF alternative is documented in section 9 and is honest about its cost.

**What has to be open, on the normal path:** nothing new. The agent already talks to the manager on
**1514/TCP** (events) and used **1515/TCP** once to enrol. This integration adds a file on the agent
and a rules file on the manager, and no network change at all. If you are checking anyway:

```powershell
Test-NetConnection 192.168.88.4 -Port 1514        # Windows
```

```bash
ss -tnp | grep 1514                               # Linux agent
```

## 2. Make the gateway emit

The `audit:` block of the gateway's configuration, with a full example in
[`examples/gateway-audit.yaml`](examples/gateway-audit.yaml):

```yaml
audit:
  jsonl:
    enabled: true
    path: C:\ProgramData\Sentin-NPU\audit.jsonl     # absolute, always
```

Configuration lives at `C:\ProgramData\Sentin-NPU\config.yaml` on Windows and at
`~/.config/sentin-npu/config.yaml` or `/etc/sentin-npu/config.yaml` on Linux, depending on how it
was installed. On Windows the settings console (`sentin-ui.exe`, in the Start Menu) edits the same
values without opening YAML, elevates itself when it needs to, and restarts the service for you.

**Use an absolute path.** A relative one resolves against the gateway's working directory - for a
Windows service, `C:\Windows\system32` - so the file appears somewhere nobody is watching while
every check reports success. This is the trap this project has hit more often than any other.

Restart the gateway and confirm the file exists and grows:

```powershell
Restart-Service SentinNPU
Get-Item C:\ProgramData\Sentin-NPU\audit.jsonl | Select-Object Length, LastWriteTime
```

A clean request emits **nothing** - deliberately, because a SIEM full of events about nothing buries
the real ones. So send something that will be found, through the gateway:

```powershell
curl.exe -s http://localhost:4141/openai/v1/chat/completions `
  -H "Content-Type: application/json" `
  -d '{\"model\":\"gpt-4o-mini\",\"messages\":[{\"role\":\"user\",\"content\":\"PESEL 02250514465\"}]}'
Get-Content C:\ProgramData\Sentin-NPU\audit.jsonl -Tail 3
```

`02250514465` is a synthetic number with a valid check digit - the detector verifies arithmetic, so
an invented number that does not check out proves nothing. Note that the identifier does **not**
appear in the events it produces: that is the schema working, not a failure.

## 3. Collect it on the agent

**Option A, one machine.** Paste the block from
[`examples/localfile-windows.conf`](examples/localfile-windows.conf) or
[`examples/localfile-linux.conf`](examples/localfile-linux.conf) into that agent's own
`ossec.conf`, inside `<ossec_config>`, then restart the agent:

```powershell
Restart-Service WazuhSvc                    # Windows, elevated
```

```bash
sudo systemctl restart wazuh-agent          # Linux
```

**Option B, from the manager, and the one to prefer past a couple of machines.** It also works
where you cannot edit `ossec.conf` at all, which on Windows means any machine where you do not have
local administrator rights. Install
[`examples/agent-group.conf`](examples/agent-group.conf) as the group's `agent.conf`:

```bash
sudo /var/ossec/bin/agent_groups -a -g sentin-npu -q
sudo /var/ossec/bin/agent_groups -a -i <AGENT_ID> -g sentin-npu -q

sudo cp agent-group.conf /var/ossec/etc/shared/sentin-npu/agent.conf
sudo chown wazuh:wazuh /var/ossec/etc/shared/sentin-npu/agent.conf
sudo chmod 660 /var/ossec/etc/shared/sentin-npu/agent.conf
sudo /var/ossec/bin/verify-agent-conf
sudo /var/ossec/bin/agent_control -R -u <AGENT_ID>      # push now instead of waiting
```

That last line is not an optimisation, it is a step of the procedure. The agent downloads the new
shared configuration within a minute and then goes on following the **old** file until it restarts,
reporting zero events and zero drops, which looks exactly like a quiet system.

**Do not leave a backup in the group directory.** The manager merges every file in
`/var/ossec/etc/shared/<group>/` into `merged.mg`, so `agent.conf.bak` is not a backup: it is a
second live configuration, and the agent will collect both paths. Keep backups elsewhere. This one
has already cost a working day on a live manager.

A group is worth the extra step even for a single agent: the change sits in one reviewable place, it
touches no other agent, and removing the group removes the integration.

**Now check that the agent is really reading the file**, before going anywhere near the manager:

```powershell
Get-Content "C:\Program Files (x86)\ossec-agent\wazuh-logcollector.state"     # Windows
```

```bash
grep -A3 audit.jsonl /var/ossec/var/run/wazuh-logcollector.state              # Linux
```

The file must appear there with a rising `events` count and `drops` at zero. On Windows that state
file is readable without elevation even when `ossec.conf` is not, which makes it the quickest way to
tell "not collected" from "collected, but no rule matched" - two states that look identical from the
dashboard.

## 4. Install the rules on the manager

Either run the script, which does this section and the group from section 3, idempotently:

```bash
sudo ./deploy-manager.sh --dry-run        # prints every change it would make
sudo ./deploy-manager.sh
```

Or do it by hand:

```bash
sudo install -o wazuh -g wazuh -m 660 sentin_npu_rules.xml /var/ossec/etc/rules/
sudo /var/ossec/bin/wazuh-analysisd -t          # must exit 0 BEFORE you restart anything
sudo systemctl restart wazuh-manager
```

`wazuh-analysisd -t` parses the whole ruleset without touching the running service. On a busy
manager this matters: a syntax error found here costs a second, and the same error found after a
restart costs however long it takes somebody to notice that alerts stopped.

**Then test against real lines, which is the step that catches a schema drift.** Copy
[`examples/sample-audit.jsonl`](examples/sample-audit.jsonl) to the manager and feed it in:

```bash
sudo /var/ossec/bin/wazuh-logtest
# paste one line, then read:
#   Phase 2: decoder 'json'
#   Phase 3: rule '100503' level 9
```

Expected verdict for each sample line, in order:

| Line | Event | Rule | Level |
|---|---|---|---|
| 1 | `gateway_start` | 100522 | 3 |
| 2, 3 | PESEL and IBAN `blocked` | 100501 | 12 |
| 4 | `decision_made` / `blocked` | 100510 | 10 |
| 5 | PESEL `masked` | 100503 | 9 |
| 6 | EMAIL `masked` | 100502 | 7 |
| 7 | PERSON `advised` | 100504 | 4 |
| 8 | LOCATION `observed` | 100505 | 3 |
| 9 | `decision_made` / `masked` | 100511 | 5 |
| 10 | IBAN `advised` inside a PDF | 100507 | 10 |
| 11 | `attachment_skipped` | 100524 | 6 |
| 12 | `inspection_timeout` | 100520 | 8 |
| 13 | `device_fallback` | 100521 | 5 |
| 14 | `gateway_stop` | 100523 | 7 |

The three frequency rules (100525, 100530, 100531) cannot be tested this way: they need repetition
inside a time window, and `wazuh-logtest` evaluates one line at a time.

**If ids 100500-100531 are already taken on your manager**, renumber the file and change the rule-id
constants at the top of `build_dashboard.py`, then regenerate the ndjson. Five panels filter by rule
id and would otherwise be empty.

**If you ever add an event kind to the gateway, add it to rule 100500 as well.** It is the parent
every other rule hangs off, and a child whose parent never matches never fires, silently. That is
not hypothetical: `attachment_skipped` was added to the gateway and not to that line, and rule
100524 was dead from the day it was written until somebody counted the alerts and found none.

## 5. Import the dashboard

Dashboards -> **Stack Management** -> **Saved Objects** -> **Import** ->
`sentin-npu-dashboard.ndjson` -> *Automatically overwrite conflicts*. Sixteen objects are created,
all named with the `sentin-npu-` prefix.

The panels reference the alerts index pattern by the id `wazuh-alerts-*`, which is what a stock
Wazuh install uses. If yours differs, regenerate rather than repairing fifteen panels by hand:

```bash
python3 build_dashboard.py --index-pattern 'your-pattern-id'
```

Open **Sentin-NPU - data leaving for LLMs**. The panels pin no time range, so a dashboard showing
nothing is usually the time picker rather than a broken import - check that first, always.

## 6. Verify it end to end

The only check that counts is an alert raised by a real request. From the machine running the
gateway:

```powershell
curl.exe -s http://localhost:4141/openai/v1/chat/completions `
  -H "Content-Type: application/json" `
  -d '{\"model\":\"gpt-4o-mini\",\"messages\":[{\"role\":\"user\",\"content\":\"PESEL 02250514465, IBAN PL61109010140000071219812874\"}]}'
```

Then, on the manager:

```bash
sudo grep -c sentin /var/ossec/logs/alerts/alerts.json
sudo tail -n 40 /var/ossec/logs/alerts/alerts.json | grep -o '"id":"1005[0-9][0-9]"' | sort | uniq -c
```

You should see 100501 or 100503 for the identifiers, and 100510 or 100511 for the request. Then find
the same alerts in the dashboard, which confirms the indexer and the panels, not just the manager.

Work through it in this order when something is missing, because each step rules out everything
below it:

1. Does `audit.jsonl` grow when you send a request? If not, the gateway is the problem, not Wazuh.
2. Does `wazuh-logcollector.state` show the file with a rising count? If not, the agent is not
   reading it - path, or a restart that never happened.
3. Does `wazuh-logtest` match a line from that file? If not, it is the rules.
4. Do alerts appear in `alerts.json`? If yes but the dashboard is empty, it is the time range or the
   index pattern.

## 7. What you will see, and what it means

| Rule | Level | Fires on |
|---|---|---|
| 100500 | 0 | The anchor. Never alerts; matching it proves the line was decoded as our JSON |
| 100501 | 12 | An identifier was **blocked** before leaving the machine |
| 100502 | 7 | An identifier was **masked** before leaving the machine |
| 100503 | 9 | As above, for PESEL, IBAN or a payment card - individually reportable |
| 100504 | 4 | A finding the user was advised about and was free to ignore |
| 100505 | 3 | Observed only |
| 100506 | 6 | An advised finding **inside an attachment** |
| 100507 | 10 | A high-value identifier inside an attachment |
| 100510 | 10 | A whole request refused by policy. Somebody was told no and will ask why |
| 100511 | 5 | A whole request forwarded with identifiers masked |
| 100520 | 8 | Inspection did not finish; that traffic may have left **uninspected** |
| 100521 | 5 | Inference fell back to another device |
| 100522 | 3 | Gateway started |
| 100523 | 7 | Gateway stopped; inspection is no longer in the path |
| 100524 | 6 | An attachment could not be read, so it left uninspected |
| 100525 | 10 | Six uninspected attachments in ten minutes |
| 100530 | 12 | Eight identifiers masked in five minutes - a habit rather than an accident |
| 100531 | 13 | Four requests blocked in ten minutes - somebody is testing the policy |

Severity follows what an operator can act on rather than what sounds dramatic. A blocked request is
the loudest, because a person was refused. Advised and observed are layer-2 opinions the user could
ignore, and a SOC that pages on those learns to ignore the source.

**Fields to query.** All of them arrive under `data.`, straight from the JSON;
[`../events.md`](../events.md) is the authoritative reference and any change to the schema updates
it in the same commit. The pair most often confused is `data.model_id` (the NER model doing the
**inspecting**) against `data.upstream_model` (the model the data was about to be **sent to**). Group
"where is our data going" by the second.

**Events never contain the detected text.** No field is capable of holding it. Where content matters
for correlation, `content_sha256` covers the whole inspected payload rather than the identifier, so
it cannot be brute-forced back to an eleven-digit number. That property is what makes it defensible
to point this trail at a SOC that many people can read.

**`client_addr` is personal data** in most deployments, exactly as any proxy log is. It is recorded
because a decision without an owner cannot be acted on. If your retention rules forbid it, turn the
sink off rather than filtering the field: the same value reaches every emitter.

### Three things that are true today and will surprise you

Written down because each one looks like a broken deployment and is not:

- **`gateway_stop` is never emitted.** The event kind exists, `docs/events.md` lists it and rule
  100523 waits for it, but no code path in the gateway produces one - so that rule cannot fire. The
  sample file carries the line so the rule can be tested; do not use a missing 100523 as evidence
  that a gateway is still running.
- **Rule 100521's description names no device.** `device_fallback` carries the pair as
  `data.detail.requested` and `data.detail.actual`, not in the top-level `device` field the
  description substitutes, so the text reads "now" and stops. The information is in the alert, one
  level down.
- **Rule 100524's description names no model.** `attachment_skipped` carries no `upstream_model`,
  so "towards" is followed by nothing.

### A limitation of the two repetition rules

100530 and 100531 count **per agent**, not per source address. Wazuh 4.14 stops a frequency rule
firing at all as soon as any `same_*` constraint is added - tested with `same_field` on both
`client_addr` and `data.client_addr`, and with `same_location`, against a control rule that fires
reliably without one. A rule that silently never fires is worse than a coarser one that does.

In practice the two are nearly the same thing, because a gateway runs on one workstation whose agent
reports one machine. On a **shared** gateway serving many callers they are not: read the address in
the alert rather than trusting the grouping, and remember that the description names the address of
the event that tripped the threshold, not the only one that contributed.

## 8. Troubleshooting

**Nothing in the dashboard.** In this order, because the first answer is right more often than
everything below it combined:

1. Time range.
2. Is the agent reading the file? `wazuh-logcollector.state`, as in section 3.
3. Are alerts being written? `grep sentin /var/ossec/logs/alerts/alerts.json`.
4. Is the event arriving but matching nothing? **A log that matches no rule is dropped silently** -
   no alert, and no archive unless `logall_json` is on. An empty dashboard with a healthy collector
   means exactly this. `wazuh-logtest` answers it in seconds.
5. The index pattern id in the import. A panel with a broken reference renders empty rather than
   erroring.

**Alerts arrive at level 3 with a generic description.** Something else matched first, or 100500 did
not. Check that the JSON decoder ran - `Phase 2: decoder 'json'` in `wazuh-logtest`. If the line was
collected as `syslog` rather than `json`, the fields are not there to match on, and `log_format` in
the `localfile` block is wrong.

**The rules do not fire after editing them.** The manager must be restarted; `wazuh-analysisd -t`
only validates. And `<if_sid>` resolves in file order, so a child defined before its parent is dead.

**Everything worked, then stopped after a rotation.** The collector follows a truncation in place.
If your rotation renames and recreates instead, the events written between the two are lost. Use
`copytruncate` - see [`examples/logrotate-sentin-npu.conf`](examples/logrotate-sentin-npu.conf) and
[`examples/rotate-audit.ps1`](examples/rotate-audit.ps1).

**Events stopped after a gateway upgrade.** Check `audit.jsonl.path` in the configuration the
upgraded gateway is actually reading. An installer that writes `config.yaml.new` beside an existing
`config.yaml` has deliberately not changed your settings - but if somebody adopted the new file, the
path may have moved.

**The events carry no `device` and no `model_id`.** That is not a Wazuh problem: the gateway is
running with layer 2 unavailable, so only the checksum detectors are working. Look for
`layer 2 ready` in `C:\ProgramData\Sentin-NPU\sentin-gateway.log`. It is worth an alert of your own:
a gateway inspecting half of what it claims looks healthy from every other angle.

## 9. The CEF alternative

If reading a file is impossible, the gateway also emits CEF over syslog:

```yaml
audit:
  syslog_cef:
    enabled: true
    address: 192.168.88.4:514
    protocol: udp
```

Fields land as CEF extensions: `src` and `spt` for the caller, `cs5` upstream model, `cs6` provider,
`cs1` detector, `cs2` data type, `cs3` model id, `cs4` device, `act` decision, `dhost` upstream host,
`fileHash` content digest, and the attachment fields map onto `cat`, `fileType` and `fsize`.

**The rules in this integration match JSON field names and will not fire on CEF.** You would need a
decoder of your own. That is the honest reason the file route is recommended rather than merely
preferred.

OTLP is also available and is out of scope here: Wazuh has no OTLP receiver.

## 10. Removing it

```bash
sudo rm /var/ossec/etc/rules/sentin_npu_rules.xml
sudo /var/ossec/bin/agent_groups -r -i <AGENT_ID> -g sentin-npu -q
sudo systemctl restart wazuh-manager
```

Then delete the saved objects in Dashboards - they are all prefixed `sentin-npu-` - and remove the
`<localfile>` block from the agent if you added it by hand in Option A.

Turning the gateway's JSONL sink off is a separate decision: the file is also what the settings
console builds its offline HTML report from, on machines that have no SIEM at all.
