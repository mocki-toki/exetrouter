#!/usr/bin/env python3
"""Explicit private numeric export through passive exr Doctor/QuotaHistory reads."""
import argparse
import json
import math
import os
from pathlib import Path
import subprocess
import time


def read(exr, args):
    result = subprocess.run([exr, *args, "--json"], stdin=subprocess.DEVNULL,
                            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                            env={**os.environ,"EXR_NO_UPDATE_CHECK":"1"}, timeout=65)
    # Doctor can return useful JSON with a nonzero diagnostic status.
    if (result.returncode and args[0] != "doctor") or len(result.stdout) > 8 * 1024 * 1024:
        raise ValueError("passive read failed")
    return json.loads(result.stdout)


def collect(exr, period, requested=None):
    if period not in ("7d", "30d", "90d"):
        raise ValueError("invalid period")
    if requested is None:
        value = read(exr,["doctor"])
        report = value.get("server",value)
        requested = [r["id"] for r in report.get("quota_accounts",[])]
    if not requested or len(requested)>128 or any(type(i) is not int or i<0 for i in requested):
        raise ValueError("invalid account selection")
    accounts = []
    for index, identifier in enumerate(sorted(set(requested)),1):
        h = read(exr,["quota-history",str(identifier),"--period",period])
        if h["interval_seconds"] != 600 or len(h["points"])>25920:
            raise ValueError("unsupported history")
        points = []
        for raw in h["points"]:
            p = {key:raw.get(key) for key in ("kind","hour","observed_at","used_percent","window_minutes","reset_at")}
            if (p["kind"] not in ("primary","secondary")
                    or any(type(p[k]) is not int or p[k]<0 for k in ("hour","observed_at"))
                    or p["hour"]%600 or not p["hour"]<=p["observed_at"]<p["hour"]+600
                    or not isinstance(p["used_percent"],(int,float)) or not math.isfinite(p["used_percent"]) or p["used_percent"]<0
                    or any(p[k] is not None and (type(p[k]) is not int or p[k]<0) for k in ("window_minutes","reset_at"))):
                raise ValueError("invalid numeric history")
            points.append(p)
        accounts.append({"account":f"account-{index:03}","interval_seconds":600,"points":points})
    return {"schema_version":1,"collected_at":int(time.time()),"accounts":accounts}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exr",default="exr")
    parser.add_argument("--period",choices=("7d","30d","90d"),default="90d")
    parser.add_argument("--account",action="append",type=int)
    parser.add_argument("--output",required=True,type=Path)
    args = parser.parse_args()
    try:
        data = collect(args.exr,args.period,args.account)
        fd = os.open(args.output,os.O_CREAT | os.O_EXCL | os.O_WRONLY,0o600)
        with os.fdopen(fd,"w") as file:
            json.dump(data,file,allow_nan=False)
            file.write("\n")
    except (ValueError, KeyError, TypeError, OSError, subprocess.TimeoutExpired):
        raise SystemExit("Export failed. Check the saved exr profile and private output path; no service changes were attempted.") from None
    print(f"Exported numeric history for {len(data['accounts'])} anonymized accounts; no upstream refresh requested.")


if __name__ == "__main__":
    main()
