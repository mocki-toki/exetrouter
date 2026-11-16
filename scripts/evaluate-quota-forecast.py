#!/usr/bin/env python3
"""Evaluate private Rust replay output; no network, inference or runtime writes.

The interval-linear comparator is a downsampled proxy, NOT the legacy TUI
algorithm: ten-minute history cannot reconstruct its sub-minute observations.
"""
import argparse
import collections
import json
import math
import os
from pathlib import Path
import statistics

HORIZONS = (600, 1800, 3600, 7200)
LABEL_RESET_TOLERANCE = 60  # Fixed outcome labels, independent of estimator candidates.


def valid(p):
    return (p.get("window_minutes") is not None and p["window_minutes"] > 0
            and p.get("reset_at") is not None and p["reset_at"] > p["observed_at"]
            and math.isfinite(p["used_percent"]) and p["used_percent"] >= 0)


def connected(a, b, anchor):
    return (valid(a) and valid(b) and b["hour"] == a["hour"] + 600
            and 0 < b["observed_at"] - a["observed_at"] < 1200
            and a["window_minutes"] == b["window_minutes"]
            and abs(b["reset_at"] - anchor) <= LABEL_RESET_TOLERANCE
            and b["used_percent"] >= a["used_percent"])


def episodes(rows):
    result = []
    for row in rows:
        p = row["point"]
        if not valid(p):
            result.append([])
            continue
        if not result or not result[-1] or not connected(result[-1][-1]["point"], p, result[-1][0]["point"]["reset_at"]):
            result.append([])
        result[-1].append(row)
    return [episode for episode in result if episode]


def distribution(values):
    if not values:
        return {"count": 0, "mean": None, "median": None, "p90": None}
    ordered = sorted(values)
    return {"count": len(values), "mean": statistics.fmean(values),
            "median": statistics.median(values), "p90": ordered[math.ceil((len(ordered) - 1) * .9)]}


def proxy(previous, p):
    if previous is None:
        return None
    seconds = p["observed_at"] - previous["observed_at"]
    delta = p["used_percent"] - previous["used_percent"]
    if seconds <= 0 or delta <= 0:
        return None
    rate = delta * 3600 / seconds
    eta = p["observed_at"] + math.ceil((100 - p["used_percent"]) / rate * 3600)
    return {"percent_per_hour": rate, "limit_at": eta if eta < p["reset_at"] else None}


def metrics():
    return {"eligible_points": 0, "estimates": 0, "limit_predictions": 0,
            "horizon_errors_pp": {str(h): [] for h in HORIZONS},
            "eta_interval_errors_minutes": [], "eta_errors_by_event": collections.defaultdict(list),
            "observed_depletion_evaluable": 0, "observed_depletion_missed": 0,
            "deadline_jumps_minutes": []}


def finalize(m):
    return {**{k: v for k, v in m.items() if not isinstance(v, (list, dict))},
            "horizon_errors_pp": {h: distribution(v) for h, v in m["horizon_errors_pp"].items()},
            "eta_interval_errors_minutes": distribution(m["eta_interval_errors_minutes"]),
            "event_macro_eta_error_minutes": distribution([statistics.fmean(v) for v in m["eta_errors_by_event"].values()]),
            "deadline_jumps_minutes": distribution(m["deadline_jumps_minutes"])}


def interval_error(eta, lower, upper):
    return max(lower - eta, eta - upper, 0) / 60


def evaluate(rows, train_fraction=2 / 3):
    if not 0.2 <= train_fraction <= 0.8:
        raise ValueError("invalid split")
    buckets = sorted({r["point"]["hour"] for r in rows if valid(r["point"])})
    if len(buckets) < 6:
        raise ValueError("insufficient history")
    split = buckets[int(len(buckets) * train_fraction)]
    groups = collections.defaultdict(list)
    for r in rows:
        groups[(r["account"], r["point"]["kind"])].append(r)
    results = {}
    quality = collections.Counter({name: 0 for name in ("adjacent_ten_minute_pairs", "adjacent_pairs_with_subminute_spacing", "nonexhausted_nondecreasing_pairs", "pairs_with_small_reset_jitter", "floating_unused_reset_pairs")})
    event = 0
    for series in groups.values():
        series.sort(key=lambda r: r["point"]["observed_at"])
        for a, b in zip(series, series[1:]):
            x, y = a["point"], b["point"]
            if y["hour"] == x["hour"] + 600:
                quality["adjacent_ten_minute_pairs"] += 1
                if y["observed_at"] - x["observed_at"] <= 60:
                    quality["adjacent_pairs_with_subminute_spacing"] += 1
                if valid(x) and valid(y) and y["used_percent"] >= x["used_percent"] and y["used_percent"] < 100:
                    drift = abs(y["reset_at"] - x["reset_at"])
                    quality["nonexhausted_nondecreasing_pairs"] += 1
                    if 0 < drift <= 5:
                        quality["pairs_with_small_reset_jitter"] += 1
                    if x["used_percent"] == y["used_percent"] == 0 and drift > 60:
                        quality["floating_unused_reset_pairs"] += 1
        for episode in episodes(series):
            event += 1
            end = next((i for i, r in enumerate(episode) if r["point"]["used_percent"] >= 100), None)
            previous_estimates = None
            previous_partition = None
            was_warning = False
            for i, row in enumerate(episode):
                p = row["point"]
                if p["used_percent"] >= 100:
                    continue
                partition = "training" if p["hour"] < split else "validation"
                tier = "weekly" if p["window_minutes"] >= 10080 else "short"
                key = f"{partition}/{tier}"
                if key not in results:
                    results[key] = {"models": {name: metrics() for name in ("runtime_current", "runtime_usual", "interval_linear_proxy")},
                                    "paired_horizon_errors_pp": {str(h): {"runtime_current": [], "interval_linear_proxy": []} for h in HORIZONS},
                                    "paired_eta_interval_errors_minutes": {"runtime_current": [], "interval_linear_proxy": []},
                                    "paired_deadline_jumps_minutes": {"runtime_current": [], "interval_linear_proxy": []},
                                    "warnings": 0, "warning_starts": 0, "warnings_with_followup": 0, "followup_still_fast": 0,
                                    "depletion_events": set(), "depletion_bracket_minutes": []}
                result = results[key]
                f = row.get("forecast") or {}
                estimates = {"runtime_current": f.get("current"), "runtime_usual": f.get("usual"),
                             "interval_linear_proxy": proxy(episode[i-1]["point"] if i else None, p)}
                # Training labels stop at the boundary; validation may use all prior history.
                future = [r["point"] for r in episode[i+1:] if partition == "validation" or r["point"]["hour"] < split]
                targets = {}
                for h in HORIZONS:
                    targets[h] = next((q for q in future if h <= q["observed_at"] - p["observed_at"] < h + 1200
                                       and q["observed_at"] < p["reset_at"]), None)
                bracket = None
                if end is not None and end > i and (partition == "validation" or episode[end]["point"]["hour"] < split):
                    lower, upper = episode[end-1]["point"]["observed_at"], episode[end]["point"]["observed_at"]
                    if upper < p["reset_at"]:
                        bracket = (lower, upper)
                        if event not in result["depletion_events"]:
                            result["depletion_events"].add(event)
                            result["depletion_bracket_minutes"].append((upper-lower)/60)
                for name, estimate in estimates.items():
                    m = result["models"][name]
                    m["eligible_points"] += 1
                    if estimate is None:
                        continue
                    m["estimates"] += 1
                    eta = estimate["limit_at"]
                    m["limit_predictions"] += eta is not None
                    for h, target in targets.items():
                        if target is None:
                            continue
                        elapsed = target["observed_at"] - p["observed_at"]
                        predicted = min(100, p["used_percent"] + estimate["percent_per_hour"] * elapsed / 3600)
                        m["horizon_errors_pp"][str(h)].append(abs(predicted - min(100, target["used_percent"])))
                    if bracket:
                        m["observed_depletion_evaluable"] += 1
                        if eta is None:
                            m["observed_depletion_missed"] += 1
                        else:
                            error = interval_error(eta, *bracket)
                            m["eta_interval_errors_minutes"].append(error)
                            m["eta_errors_by_event"][event].append(error)
                    if previous_partition == partition and previous_estimates and previous_estimates[name] is not None and eta is not None and previous_estimates[name]["limit_at"] is not None:
                        m["deadline_jumps_minutes"].append(abs(eta-previous_estimates[name]["limit_at"])/60)
                if estimates["runtime_current"] and estimates["interval_linear_proxy"]:
                    for h, target in targets.items():
                        if target is None:
                            continue
                        elapsed = target["observed_at"] - p["observed_at"]
                        for name in ("runtime_current", "interval_linear_proxy"):
                            predicted = min(100, p["used_percent"] + estimates[name]["percent_per_hour"] * elapsed / 3600)
                            result["paired_horizon_errors_pp"][str(h)][name].append(abs(predicted-min(100,target["used_percent"])))
                    if all(estimates[name]["limit_at"] is not None for name in ("runtime_current", "interval_linear_proxy")):
                        if bracket:
                            for name in ("runtime_current", "interval_linear_proxy"):
                                result["paired_eta_interval_errors_minutes"][name].append(interval_error(estimates[name]["limit_at"], *bracket))
                        if previous_partition == partition and previous_estimates and all(previous_estimates[name] is not None and previous_estimates[name]["limit_at"] is not None for name in ("runtime_current", "interval_linear_proxy")):
                            for name in ("runtime_current", "interval_linear_proxy"):
                                result["paired_deadline_jumps_minutes"][name].append(abs(estimates[name]["limit_at"]-previous_estimates[name]["limit_at"])/60)
                warning = f.get("warning") is not None
                if warning:
                    result["warnings"] += 1
                    result["warning_starts"] += not was_warning
                    q = targets[1800]
                    if q is not None and f.get("usual") is not None:
                        result["warnings_with_followup"] += 1
                        consumed = min(100,q["used_percent"]) - p["used_percent"]
                        future_rate = consumed * 3600 / (q["observed_at"]-p["observed_at"])
                        usual = f["usual"]["percent_per_hour"]
                        result["followup_still_fast"] += consumed > 0 and future_rate >= 1.5 * usual
                was_warning = warning
                previous_estimates, previous_partition = estimates, partition
    return {"method": "chronological-runtime-replay-v1", "split_utc": split,
            "split_basis": "chronological split over unique observed UTC buckets", "train_fraction": train_fraction,
            "accounts": len({r["account"] for r in rows}), "observations": len(rows),
            "span_days": (max(buckets)-min(buckets))/86400, "data_quality": dict(quality),
            "limitations": ["The interval-linear proxy is not the legacy sub-minute TUI estimator.",
                            "Hourly-era gaps are not interpolated into ten-minute observations.",
                            "Depletion is interval-censored; reset or disappearance alone is not exhaustion.",
                            "Warning continuation is not a ground-truth false-positive label.",
                            "Overlapping observations are correlated; these are not confidence intervals."],
            "results": {key: {**{k: v for k,v in result.items() if k not in ("models", "paired_horizon_errors_pp", "paired_eta_interval_errors_minutes", "paired_deadline_jumps_minutes", "depletion_events", "depletion_bracket_minutes")},
                              "depletion_events": len(result["depletion_events"]),
                              "depletion_bracket_minutes": distribution(result["depletion_bracket_minutes"]),
                              "models": {name: finalize(m) for name,m in result["models"].items()},
                              "paired_eta_interval_errors_minutes": {name: distribution(v) for name,v in result["paired_eta_interval_errors_minutes"].items()},
                              "paired_deadline_jumps_minutes": {name: distribution(v) for name,v in result["paired_deadline_jumps_minutes"].items()},
                              "paired_horizon_errors_pp": {h: {name: distribution(v) for name,v in models.items()} for h,models in result["paired_horizon_errors_pp"].items()}}
                        for key,result in sorted(results.items())}}


def load(path):
    if path.is_symlink() or not path.is_file() or path.stat().st_size > 64 * 1024 * 1024 or path.stat().st_mode & 0o077:
        raise ValueError("input must be a private replay file")
    rows = []
    seen = set()
    for line in path.read_text().splitlines():
        r = json.loads(line)
        alias = r["account"]
        suffix = alias.removeprefix("account-")
        p = r["point"]
        if not alias.startswith("account-") or not suffix.isascii() or not suffix.isdecimal() or len(suffix)>16 or p["kind"] not in ("primary","secondary"):
            raise ValueError("invalid replay")
        key = alias,p["kind"],p["hour"]
        if key in seen:
            raise ValueError("duplicate observation")
        seen.add(key)
        rows.append(r)
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--replay", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--train-fraction", type=float, default=2/3)
    args = parser.parse_args()
    try:
        report = evaluate(load(args.replay), args.train_fraction)
        fd = os.open(args.output, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        with os.fdopen(fd,"w") as file:
            json.dump(report,file,indent=2,allow_nan=False)
            file.write("\n")
    except (ValueError, KeyError, TypeError, OSError):
        raise SystemExit("Evaluation failed. Check private numeric replay data and output paths.") from None
    print("Offline evaluation completed; no service contacted.")


if __name__ == "__main__":
    main()
