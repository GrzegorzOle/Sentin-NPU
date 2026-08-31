#!/bin/sh
# Copyright 2026 Grzegorz Oleksy
# SPDX-License-Identifier: Apache-2.0
#
# Install the Sentin-NPU ruleset on a Wazuh manager, and optionally create the agent group that
# ships the events. Idempotent: run it twice and the second run changes nothing.
#
#   sudo ./deploy-manager.sh                     # rules only
#   sudo ./deploy-manager.sh --agent 005         # rules, plus a group for agent 005
#   sudo ./deploy-manager.sh --agent 005 --path 'C:\ProgramData\sentin-npu\audit.jsonl'
#   sudo ./deploy-manager.sh --dry-run           # print what would happen
#
# It refuses to restart the manager unless the ruleset validates, because a manager that will not
# start is a worse outcome than an integration that is not deployed yet.

set -eu

OSSEC=${OSSEC:-/var/ossec}
GROUP=sentin-npu
AGENT=""
LOGPATH="/var/log/sentin-npu/audit.jsonl"
DRY=0
HERE=$(cd "$(dirname "$0")" && pwd)

while [ $# -gt 0 ]; do
    case "$1" in
        --agent) AGENT=${2:?--agent needs an agent id}; shift 2 ;;
        --path) LOGPATH=${2:?--path needs a path}; shift 2 ;;
        --group) GROUP=${2:?--group needs a name}; shift 2 ;;
        --dry-run) DRY=1; shift ;;
        -h|--help) sed -n '3,20p' "$0"; exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done

run() {
    if [ "$DRY" -eq 1 ]; then
        printf '  would run: %s\n' "$*"
    else
        "$@"
    fi
}

say() { printf '\n== %s\n' "$1"; }

[ -d "$OSSEC" ] || { echo "no Wazuh install at $OSSEC - set OSSEC=/path" >&2; exit 1; }
[ "$(id -u)" -eq 0 ] || [ "$DRY" -eq 1 ] || { echo "run as root" >&2; exit 1; }

say "rules"
if [ -f "$OSSEC/etc/rules/sentin_npu_rules.xml" ] &&
   cmp -s "$HERE/sentin_npu_rules.xml" "$OSSEC/etc/rules/sentin_npu_rules.xml"; then
    echo "  already installed and identical"
    RULES_CHANGED=0
else
    run install -o wazuh -g wazuh -m 660 \
        "$HERE/sentin_npu_rules.xml" "$OSSEC/etc/rules/sentin_npu_rules.xml"
    echo "  installed $OSSEC/etc/rules/sentin_npu_rules.xml"
    RULES_CHANGED=1
fi

if [ -n "$AGENT" ]; then
    say "agent group '$GROUP' for agent $AGENT"
    if [ -d "$OSSEC/etc/shared/$GROUP" ]; then
        echo "  group exists"
    else
        run "$OSSEC/bin/agent_groups" -a -g "$GROUP" -q
    fi

    if [ "$DRY" -eq 1 ]; then
        printf '  would write %s/etc/shared/%s/agent.conf collecting %s\n' "$OSSEC" "$GROUP" "$LOGPATH"
    else
        cat > "$OSSEC/etc/shared/$GROUP/agent.conf" <<EOF
<agent_config>
  <localfile>
    <location>$LOGPATH</location>
    <log_format>json</log_format>
  </localfile>
</agent_config>
EOF
        chown wazuh:wazuh "$OSSEC/etc/shared/$GROUP/agent.conf"
        chmod 660 "$OSSEC/etc/shared/$GROUP/agent.conf"
        echo "  collecting $LOGPATH"
    fi

    run "$OSSEC/bin/agent_groups" -a -i "$AGENT" -g "$GROUP" -q
    run "$OSSEC/bin/verify-agent-conf"
fi

say "validating the ruleset"
if [ "$DRY" -eq 1 ]; then
    echo "  would run $OSSEC/bin/wazuh-analysisd -t"
elif "$OSSEC/bin/wazuh-analysisd" -t 2>&1 | grep -iE '^.*ERROR' ; then
    echo "  ruleset has errors - NOT restarting the manager" >&2
    exit 1
else
    echo "  ruleset parses"
fi

say "restart"
if [ "$RULES_CHANGED" -eq 0 ] && [ -z "$AGENT" ]; then
    echo "  nothing changed, no restart needed"
else
    run systemctl restart wazuh-manager
    [ -n "$AGENT" ] && run "$OSSEC/bin/agent_control" -R -u "$AGENT"
fi

cat <<'EOF'

Next:
  1. Import sentin-npu-dashboard.ndjson in Dashboards (Stack Management -> Saved Objects).
  2. Send a request through the gateway carrying a synthetic identifier.
  3. grep sentin /var/ossec/logs/alerts/alerts.json

If step 3 is empty but the agent's wazuh-logcollector.state shows the file being read, the event
matched no rule and was dropped silently. Paste one line into wazuh-logtest; it will say why.
EOF
