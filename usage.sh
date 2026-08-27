#!/usr/bin/env bash
# How many people have clipd, and how many still run it.
#
# Two different questions with two different sources:
#
#   downloads — GitHub release assets. Needs nothing but `gh` auth. Counts
#               every fetch, including install.sh re-runs and CI, so it is an
#               upper bound on people.
#   installs  — the telemetry worker, which counts distinct install ids. The
#               worker URL is a repo secret, so pass it in or set
#               CLIPD_TELEMETRY_ENDPOINT.
#
#   ./usage.sh                       # downloads only
#   ./usage.sh https://<worker-url>  # downloads + active installs
set -euo pipefail
REPO="${CLIPD_REPO:-shwetarkadam/clipd}"
ENDPOINT="${1:-${CLIPD_TELEMETRY_ENDPOINT:-}}"

echo "── downloads (GitHub release assets) ───────────────────────"
gh api "repos/$REPO/releases" --paginate \
  --jq '.[] | "\(.tag_name)\t\([.assets[].download_count] | add // 0)"' |
  awk -F'\t' '{printf "  %-10s %s\n", $1, $2; total+=$2} END {printf "  %-10s %s\n", "TOTAL", total}'

if [ -z "$ENDPOINT" ]; then
  echo
  echo "── active installs ────────────────────────────────────────"
  echo "  no telemetry endpoint given — pass it as an argument to include this."
  exit 0
fi

echo
echo "── active installs (telemetry worker) ─────────────────────"
curl -fsS "${ENDPOINT%/}/stats" | python3 -c '
import json, sys
d = json.load(sys.stdin)
p = d.get("people", {})
s = d.get("starts", {})
print("  distinct installs ever : %s" % p.get("installs_total", 0))
print("  active today           : %s" % p.get("active_today", 0))
print("  active this month      : %s" % p.get("active_this_month", 0))
daily = p.get("daily", {})
if daily:
    days = list(daily.items())[:7]
    print("  last 7 days            : " + "  ".join("%s=%s" % (k[5:], v) for k, v in days))
print("  daemon starts (total)  : %s   # launches, not people" % s.get("total", 0))
by = s.get("by_version", {})
if by:
    top = sorted(by.items(), key=lambda kv: -kv[1])[:5]
    print("  starts by version      : " + "  ".join("%s=%s" % (k, v) for k, v in top))
'
