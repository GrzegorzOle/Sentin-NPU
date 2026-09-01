#!/usr/bin/env python3
# Copyright 2026 Grzegorz Oleksy
# SPDX-License-Identifier: Apache-2.0
"""Build the Sentin-NPU dashboard as an OpenSearch Dashboards ndjson import.

The dashboard ships as a generated artefact (`sentin-npu-dashboard.ndjson`) so an administrator
needs nothing but the import button. It is generated rather than hand-written because saved-object
JSON is unreviewable by eye: a panel change should read as a two-line diff here, not as a
reshuffled 40 KB blob.

Every panel answers a question a supervisor actually asks:

- is anyone trying to send identifiers out, and is it getting worse
- which identifiers, by type, since PESEL leaving the building is not the same as a city name
- from which workstation, because a decision without an owner cannot be acted on
- towards which model, because "our data went to a model in another country" is the finding
- and did inspection ever fail open, which is the failure that looks like success everywhere else

Usage:
    python3 build_dashboard.py [--index-pattern wazuh-alerts-*] [--output FILE]

The index pattern is referenced by id. On a stock Wazuh install the alerts pattern's id is
literally `wazuh-alerts-*`; if yours differs, pass it, or the panels will import with a broken
reference and render empty.
"""

from __future__ import annotations

import argparse
import json
from typing import Any

DASHBOARD_ID = "sentin-npu-dlp"
DASHBOARD_TITLE = "Sentin-NPU - data leaving for LLMs"

# Rule ids from sentin_npu_rules.xml. Kept here so a site that had to renumber the rules changes
# them in one place rather than in nine panel queries.
RULE_PII_BLOCKED = 100501
RULE_PII_MASKED = 100502
RULE_PII_HIGH_VALUE = 100503
RULE_REQUEST_BLOCKED = 100510
RULE_INSPECTION_GAP = 100520
RULE_ATTACHMENT_SKIPPED = 100524
RULE_REPEAT_MASKED = 100530
RULE_REPEAT_BLOCKED = 100531

SENTIN_FILTER = "rule.groups:sentin_npu"


def search_source(query: str = "") -> str:
    """The searchSourceJSON every panel carries, with the index pattern left to a reference."""
    return json.dumps(
        {
            "query": {"query": query, "language": "kuery"},
            "filter": [],
            "indexRefName": "kibanaSavedObjectMeta.searchSourceJSON.index",
        }
    )


def visualization(
    vis_id: str,
    title: str,
    description: str,
    vis_state: dict[str, Any],
    query: str = SENTIN_FILTER,
) -> dict[str, Any]:
    """One saved visualization, with its index pattern as a reference rather than an inline id."""
    return {
        "id": vis_id,
        "type": "visualization",
        "version": "1",
        "attributes": {
            "title": title,
            "description": description,
            "visState": json.dumps(vis_state),
            "uiStateJSON": "{}",
            "kibanaSavedObjectMeta": {"searchSourceJSON": search_source(query)},
        },
        "references": [
            {
                "id": "INDEX_PATTERN_ID",
                "name": "kibanaSavedObjectMeta.searchSourceJSON.index",
                "type": "index-pattern",
            }
        ],
    }


def terms_agg(field: str, size: int, order_field: str = "1") -> dict[str, Any]:
    return {
        "id": "2",
        "enabled": True,
        "type": "terms",
        "schema": "segment",
        "params": {
            "field": field,
            "orderBy": order_field,
            "order": "desc",
            "size": size,
            "otherBucket": False,
            "otherBucketLabel": "other",
            "missingBucket": False,
            "missingBucketLabel": "missing",
        },
    }


def count_agg() -> dict[str, Any]:
    return {
        "id": "1",
        "enabled": True,
        "type": "count",
        "schema": "metric",
        "params": {},
    }


def pie(vis_id: str, title: str, description: str, field: str, size: int = 12, query: str = SENTIN_FILTER):
    return visualization(
        vis_id,
        title,
        description,
        {
            "title": title,
            "type": "pie",
            "aggs": [count_agg(), terms_agg(field, size)],
            "params": {
                "type": "pie",
                "addTooltip": True,
                "addLegend": True,
                "legendPosition": "right",
                "isDonut": True,
                "labels": {"show": True, "values": True, "truncate": 100},
            },
        },
        query,
    )


def horizontal_bar(vis_id: str, title: str, description: str, field: str, size: int = 10, query: str = SENTIN_FILTER):
    return visualization(
        vis_id,
        title,
        description,
        {
            "title": title,
            "type": "horizontal_bar",
            "aggs": [count_agg(), terms_agg(field, size)],
            "params": {
                "type": "histogram",
                "grid": {"categoryLines": False},
                "categoryAxes": [
                    {
                        "id": "CategoryAxis-1",
                        "type": "category",
                        "position": "left",
                        "show": True,
                        "scale": {"type": "linear"},
                        "labels": {"show": True, "truncate": 200},
                        "title": {},
                    }
                ],
                "valueAxes": [
                    {
                        "id": "ValueAxis-1",
                        "name": "LeftAxis-1",
                        "type": "value",
                        "position": "bottom",
                        "show": True,
                        "scale": {"type": "linear", "mode": "normal"},
                        "labels": {"show": True, "rotate": 0, "filter": False, "truncate": 100},
                        "title": {"text": "events"},
                    }
                ],
                "seriesParams": [
                    {
                        "show": True,
                        "type": "histogram",
                        "mode": "normal",
                        "data": {"label": "events", "id": "1"},
                        "valueAxis": "ValueAxis-1",
                        "drawLinesBetweenPoints": True,
                        "showCircles": True,
                    }
                ],
                "addTooltip": True,
                "addLegend": False,
                "legendPosition": "right",
                "times": [],
                "addTimeMarker": False,
                "labels": {"show": True},
            },
        },
        query,
    )


def metric(vis_id: str, title: str, description: str, query: str):
    return visualization(
        vis_id,
        title,
        description,
        {
            "title": title,
            "type": "metric",
            "aggs": [count_agg()],
            "params": {
                "addTooltip": True,
                "addLegend": False,
                "type": "metric",
                "metric": {
                    "percentageMode": False,
                    "useRanges": False,
                    "colorSchema": "Green to Red",
                    "metricColorMode": "None",
                    "colorsRange": [{"from": 0, "to": 10000}],
                    "labels": {"show": True},
                    "invertColors": False,
                    "style": {"bgFill": "#000", "bgColor": False, "labelColor": False,
                              "subText": "", "fontSize": 36},
                },
            },
        },
        query,
    )


def timeline(vis_id: str, title: str, description: str, query: str = SENTIN_FILTER):
    """Events over time, split by decision - the shape that shows a habit forming."""
    return visualization(
        vis_id,
        title,
        description,
        {
            "title": title,
            "type": "histogram",
            "aggs": [
                count_agg(),
                {
                    "id": "2",
                    "enabled": True,
                    "type": "date_histogram",
                    "schema": "segment",
                    "params": {
                        "field": "timestamp",
                        "timeRange": {"from": "now-24h", "to": "now"},
                        "useNormalizedEsInterval": True,
                        "interval": "auto",
                        "drop_partials": False,
                        "min_doc_count": 1,
                        "extended_bounds": {},
                    },
                },
                {
                    "id": "3",
                    "enabled": True,
                    "type": "terms",
                    "schema": "group",
                    "params": {
                        "field": "data.decision",
                        "orderBy": "1",
                        "order": "desc",
                        "size": 5,
                        "otherBucket": False,
                        "missingBucket": False,
                    },
                },
            ],
            "params": {
                "type": "histogram",
                "grid": {"categoryLines": False},
                "categoryAxes": [
                    {
                        "id": "CategoryAxis-1",
                        "type": "category",
                        "position": "bottom",
                        "show": True,
                        "scale": {"type": "linear"},
                        "labels": {"show": True, "filter": True, "truncate": 100},
                        "title": {},
                    }
                ],
                "valueAxes": [
                    {
                        "id": "ValueAxis-1",
                        "name": "LeftAxis-1",
                        "type": "value",
                        "position": "left",
                        "show": True,
                        "scale": {"type": "linear", "mode": "normal"},
                        "labels": {"show": True, "rotate": 0, "filter": False, "truncate": 100},
                        "title": {"text": "events"},
                    }
                ],
                "seriesParams": [
                    {
                        "show": True,
                        "type": "histogram",
                        "mode": "stacked",
                        "data": {"label": "events", "id": "1"},
                        "valueAxis": "ValueAxis-1",
                        "drawLinesBetweenPoints": True,
                        "showCircles": True,
                    }
                ],
                "addTooltip": True,
                "addLegend": True,
                "legendPosition": "right",
                "times": [],
                "addTimeMarker": False,
                "labels": {"show": False},
            },
        },
        query,
    )


def table(vis_id: str, title: str, description: str, fields: list[tuple[str, int]], query: str = SENTIN_FILTER):
    """A plain table. The accessible twin of every chart above it, and what an analyst copies from."""
    aggs: list[dict[str, Any]] = [count_agg()]
    for index, (field, size) in enumerate(fields, start=2):
        aggs.append(
            {
                "id": str(index),
                "enabled": True,
                "type": "terms",
                "schema": "bucket",
                "params": {
                    "field": field,
                    "orderBy": "1",
                    "order": "desc",
                    "size": size,
                    "otherBucket": False,
                    "missingBucket": False,
                },
            }
        )
    return visualization(
        vis_id,
        title,
        description,
        {
            "title": title,
            "type": "table",
            "aggs": aggs,
            "params": {
                "perPage": 15,
                "showPartialRows": False,
                "showMetricsAtAllLevels": False,
                "showTotal": False,
                "totalFunc": "sum",
                "percentageCol": "",
            },
        },
        query,
    )


def panels() -> list[dict[str, Any]]:
    """Every panel, in reading order: how bad, of what, from whom, to where, and did we miss any."""
    return [
        metric(
            "sentin-npu-metric-blocked",
            "Blocked requests",
            "Requests refused outright. A user was told no and will ask why, so this is the number "
            "that generates a conversation.",
            f"rule.id:{RULE_REQUEST_BLOCKED}",
        ),
        metric(
            "sentin-npu-metric-high-value",
            "High-value identifiers stopped",
            "PESEL, IBAN or payment card masked before leaving. Each one is individually "
            "reportable, unlike a name or a city.",
            f"rule.id:{RULE_PII_HIGH_VALUE}",
        ),
        metric(
            "sentin-npu-metric-gaps",
            "Inspection gaps",
            "Traffic that left with part of it uninspected: inspection did not finish under a "
            "fail-open policy, or an attachment could not be read - an image, an encrypted "
            "document, something over the size limit. Should be zero; a non-zero value "
            "invalidates the other panels for that period rather than merely being untidy.",
            f"rule.id:{RULE_INSPECTION_GAP} or rule.id:{RULE_ATTACHMENT_SKIPPED}",
        ),
        metric(
            "sentin-npu-metric-repeat",
            "Repeat offenders",
            "Workstations that tripped the repetition rules. One paste is an accident; eight in "
            "five minutes is a habit, and only this panel tells them apart.",
            f"rule.id:{RULE_REPEAT_MASKED} or rule.id:{RULE_REPEAT_BLOCKED}",
        ),
        timeline(
            "sentin-npu-timeline",
            "Detections over time, by decision",
            "The shape matters more than the height: a flat line is business as usual, a step is a "
            "new integration someone pointed at the gateway, a ramp is a habit forming.",
        ),
        pie(
            "sentin-npu-data-types",
            "What was detected",
            "By data type, not by detector, so a PESEL found by layer 1 and a person found by "
            "layer 2 sit in the same picture.",
            "data.data_type",
        ),
        horizontal_bar(
            "sentin-npu-clients",
            "Which workstation",
            "Source address of the caller. This is what turns a detection into something anyone "
            "can act on. It is personal data: treat the panel accordingly.",
            "data.client_addr",
        ),
        horizontal_bar(
            "sentin-npu-models",
            "Which model the data was heading for",
            "The model the caller asked for, not the model that inspected it. A local model and a "
            "hosted one in another jurisdiction are the same row here and very different findings.",
            "data.upstream_model",
        ),
        pie(
            "sentin-npu-decisions",
            "What the gateway did",
            "Blocked, masked, advised or observed. The advisory-first design means most traffic "
            "should be masked or advised; a wall of blocks means the policy is miscalibrated.",
            "data.decision",
            size=6,
        ),
        pie(
            "sentin-npu-providers",
            "Through which provider",
            "The adapter that handled it: anthropic, openai or google. Coarser than the upstream "
            "host and stable when a router is repointed.",
            "data.provider",
            size=6,
        ),
        table(
            "sentin-npu-detail",
            "Who sent what, where",
            "The table an analyst copies from, and the accessible twin of the charts above. Read "
            "it top down: workstation, identifier type, verdict, destination model.",
            [("data.client_addr", 10), ("data.data_type", 10), ("data.decision", 5),
             ("data.upstream_model", 10)],
        ),
        table(
            "sentin-npu-device",
            "Where inspection ran",
            "The device that actually executed the NER model. Present because a gateway quietly "
            "running on the CPU after an accelerator failed still inspects, and the operator "
            "should learn that from a dashboard rather than from a latency complaint.",
            [("data.device", 5), ("data.model_id", 5)],
        ),
    ]


def dashboard(panel_ids: list[str]) -> dict[str, Any]:
    """Lay the panels out on a 48-column grid: four metrics, then the charts, then the tables."""
    layout = [
        (0, 0, 12, 8), (12, 0, 12, 8), (24, 0, 12, 8), (36, 0, 12, 8),   # metrics
        (0, 8, 48, 12),                                                   # timeline
        (0, 20, 16, 14), (16, 20, 16, 14), (32, 20, 16, 14),              # types, clients, models
        (0, 34, 24, 12), (24, 34, 24, 12),                                # decisions, providers
        (0, 46, 32, 16), (32, 46, 16, 16),                                # tables
    ]
    panels_json = []
    references = []
    for index, (panel_id, (x, y, w, h)) in enumerate(zip(panel_ids, layout), start=1):
        name = f"panel_{index}"
        panels_json.append(
            {
                "version": "2.13.0",
                "gridData": {"x": x, "y": y, "w": w, "h": h, "i": str(index)},
                "panelIndex": str(index),
                "embeddableConfig": {},
                "panelRefName": name,
            }
        )
        references.append({"id": panel_id, "name": name, "type": "visualization"})

    return {
        "id": DASHBOARD_ID,
        "type": "dashboard",
        "version": "1",
        "attributes": {
            "title": DASHBOARD_TITLE,
            "description": (
                "Identifiers detected in traffic bound for LLM providers, by type, source and "
                "destination model. Fed by Sentin-NPU audit events; see packaging/wazuh/README.md."
            ),
            "panelsJSON": json.dumps(panels_json),
            "optionsJSON": json.dumps({"hidePanelTitles": False, "useMargins": True}),
            "version": 1,
            "timeRestore": False,
            "kibanaSavedObjectMeta": {
                "searchSourceJSON": json.dumps({"query": {"query": "", "language": "kuery"}, "filter": []})
            },
        },
        "references": references,
    }


def build(index_pattern_id: str) -> str:
    objects = panels()
    objects.append(dashboard([obj["id"] for obj in objects]))

    lines = []
    for obj in objects:
        for reference in obj.get("references", []):
            if reference["id"] == "INDEX_PATTERN_ID":
                reference["id"] = index_pattern_id
        lines.append(json.dumps(obj, ensure_ascii=False))
    # The trailing summary object is what the import screen counts; without it some versions
    # report "0 of 0 imported" while having imported everything.
    lines.append(json.dumps({"exportedCount": len(objects), "missingRefCount": 0, "missingReferences": []}))
    return "\n".join(lines) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--index-pattern",
        default="wazuh-alerts-*",
        help="Saved-object id of the alerts index pattern (default: wazuh-alerts-*)",
    )
    parser.add_argument("--output", default="sentin-npu-dashboard.ndjson")
    args = parser.parse_args()

    text = build(args.index_pattern)
    with open(args.output, "w", encoding="utf-8", newline="\n") as handle:
        handle.write(text)
    print(f"wrote {args.output} ({len(text.splitlines())} objects incl. summary)")


if __name__ == "__main__":
    main()
