#!/usr/bin/env python3
"""Synthetic tests for the offline evaluator, never real account history."""
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from types import SimpleNamespace

spec = importlib.util.spec_from_file_location("evaluation", Path(__file__).with_name("evaluate-quota-forecast.py"))
evaluation = importlib.util.module_from_spec(spec)
spec.loader.exec_module(evaluation)
collector_spec = importlib.util.spec_from_file_location("collector", Path(__file__).with_name("collect-quota-history.py"))
collector = importlib.util.module_from_spec(collector_spec)
collector_spec.loader.exec_module(collector)
BASE = 1800000000


def rows():
    result = []
    for n in range(36):
        cycle, offset = divmod(n, 12)
        at = BASE+n*600
        used = min(100,offset*10)
        reset = BASE+(cycle+1)*7200
        result.append({"account":"account-001", "point":{"kind":"primary","hour":at,"observed_at":at,"used_percent":used,"window_minutes":300,"reset_at":reset},
                       "forecast":{"current":{"percent_per_hour":60,"limit_at":BASE+cycle*7200+6000},"usual":None,"warning":None}})
    return result


class EvaluationTests(unittest.TestCase):
    def test_collection_is_passive_and_discards_identity_and_unknown_payloads(self):
        point = rows()[0]["point"]
        values = [{"quota_accounts":[{"id":42,"label":"owner@example.com","secret":"private-body-marker"}]},
                  {"interval_seconds":600,"points":[{**point,"secret":"private-body-marker"}],"platform":{"secret":"private-body-marker"}}]
        with patch.object(collector.subprocess,"run",side_effect=[SimpleNamespace(returncode=0,stdout=json.dumps(v).encode()) for v in values]) as run:
            data = collector.collect("exr","90d")
        self.assertEqual([call.args[0] for call in run.call_args_list],[["exr","doctor","--json"],["exr","quota-history","42","--period","90d","--json"]])
        self.assertEqual(data["accounts"][0]["account"],"account-001")
        self.assertNotIn("owner@example.com",json.dumps(data))
        self.assertNotIn("private-body-marker",json.dumps(data))
        for call in run.call_args_list:
            self.assertEqual(call.kwargs["env"]["EXR_NO_UPDATE_CHECK"],"1")
    def test_perfect_constant_pace_has_zero_error_and_distinct_holdout(self):
        report = evaluation.evaluate(rows())
        for key in ("training/short","validation/short"):
            metrics = report["results"][key]["models"]["runtime_current"]
            self.assertGreater(metrics["horizon_errors_pp"]["1800"]["count"],0)
            self.assertEqual(metrics["horizon_errors_pp"]["1800"]["mean"],0)
            self.assertEqual(metrics["eta_interval_errors_minutes"]["mean"],0)
            self.assertEqual(metrics["deadline_jumps_minutes"]["mean"],0)
        self.assertEqual(report["results"]["training/short"]["depletion_events"],2)
        self.assertEqual(report["results"]["validation/short"]["depletion_events"],1)

    def test_validation_outcomes_cannot_change_training_scores(self):
        original = rows()
        expected = evaluation.evaluate(original)
        changed = copy.deepcopy(original)
        for row in changed:
            if row["point"]["hour"] >= expected["split_utc"]:
                row["point"]["used_percent"] = 1.0
        actual = evaluation.evaluate(changed)
        self.assertEqual(expected["results"]["training/short"],actual["results"]["training/short"])

    def test_resets_gaps_and_unknowns_are_not_depletion(self):
        original = rows()
        for r in original:
            r["point"]["used_percent"] = min(80,r["point"]["used_percent"])
        report = evaluation.evaluate(original)
        for result in report["results"].values():
            self.assertEqual(result["depletion_events"],0)
            self.assertIsNone(result["models"]["runtime_current"]["eta_interval_errors_minutes"]["mean"])
        first = original[0]["point"]
        second = {**original[1]["point"],"hour":first["hour"]+3600,"observed_at":first["observed_at"]+3600}
        self.assertFalse(evaluation.connected(first,second,first["reset_at"]))
        second = {**original[1]["point"],"reset_at":None}
        self.assertFalse(evaluation.connected(first,second,first["reset_at"]))

    def test_missing_estimates_are_unknown_not_zero(self):
        original = rows()
        for r in original:
            r["point"]["used_percent"] = 10.0
            r["forecast"] = None
        report = evaluation.evaluate(original)
        for result in report["results"].values():
            for metrics in result["models"].values():
                self.assertEqual(metrics["estimates"],0)
                self.assertIsNone(metrics["horizon_errors_pp"]["1800"]["mean"])

    def test_exhaustion_error_is_distance_to_bracket_not_an_invented_timestamp(self):
        self.assertEqual(evaluation.interval_error(BASE+300,BASE,BASE+600),0)
        self.assertEqual(evaluation.interval_error(BASE-60,BASE,BASE+600),1)
        self.assertEqual(evaluation.interval_error(BASE+1200,BASE,BASE+600),10)

    def test_only_private_pseudonymous_replay_is_loaded(self):
        with tempfile.TemporaryDirectory() as root:
            path = Path(root)/"replay.jsonl"
            path.write_text("\n".join(json.dumps(r) for r in rows()))
            path.chmod(0o600)
            self.assertEqual(len(evaluation.load(path)),36)
            path.chmod(0o644)
            with self.assertRaises(ValueError):
                evaluation.load(path)
            path.chmod(0o600)
            altered = rows()[0]
            altered["account"] = "owner@example.com"
            path.write_text(json.dumps(altered))
            with self.assertRaises(ValueError):
                evaluation.load(path)


if __name__ == "__main__":
    unittest.main()
