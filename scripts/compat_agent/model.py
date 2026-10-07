"""Responses tool loop with full transient history and no inference retries."""
import json
import os
from urllib.parse import urlsplit

from .core import DECISIONS, MODEL, Stop, decision, request

CITATION = {"type": "object", "properties": {
    "sha": {"type": "string", "pattern": "^[0-9a-f]{40}$"},
    "path": {"type": "string"}, "symbol": {"type": "string"}},
    "required": ["sha", "path", "symbol"]}
FINDING = {"type": "object", "properties": {
    "problem": {"type": "string"}, "fix": {"type": "string"}, "test": {"type": "string"},
    "upstream": {"type": "array", "items": CITATION, "minItems": 1},
    "exetrouter": {"type": "array", "items": CITATION, "minItems": 1}},
    "required": ["problem", "fix", "test", "upstream", "exetrouter"]}

SCHEMA = {
    "type": "object", "properties": {
        "decision": {"type": "string"}, "summary": {"type": "string"},
        "coverage_complete": {"type": "boolean"},
        "findings": {"type": "array", "items": FINDING},
        "title": {"type": "string"}, "body": {"type": "string"}},
    "required": ["decision", "summary", "coverage_complete", "findings"]}


def tool(name, description, properties, required):
    return {"type": "function", "name": name, "description": description,
            "strict": False,
            "parameters": {"type": "object", "properties": properties,
                           "required": required, "additionalProperties": False}}


class Model:
    def __init__(self, root, call=request):
        self.root = root
        self.base = os.environ["COMPAT_API_BASE_URL"].rstrip("/")
        parsed = urlsplit(self.base)
        if parsed.scheme != "https" or parsed.username or parsed.password or parsed.query or parsed.fragment:
            raise Stop("invalid_api_url")
        self.token = os.environ["COMPAT_API_TOKEN"]
        self.call = call
        self.calls = 0
        self.tokens = 0
        self.usage_known = True

    def preflight(self):
        models = self.call(self.base + "/models/codex", self.token)
        found = next((m for m in models.get("models", []) if m.get("slug") == MODEL), None)
        if not found:
            raise Stop("model_unavailable")
        levels = {r.get("effort") for r in found.get("supported_reasoning_levels", [])}
        if not {"medium", "xhigh"} <= levels:
            raise Stop("reasoning_levels_unavailable")

    def run(self, phase, context, sandbox, triage=None):
        prompts = self.root / "prompts/compat-agent"
        history = [{"role": "developer", "content": (prompts / "common.md").read_text()
                    + "\n" + (prompts / (phase + ".md")).read_text()},
                   {"role": "user", "content": json.dumps(context)}]
        tools = [
            tool("read_file", "Read pinned source. repository must match supplied evidence.",
                 {k: {"type": "string"} for k in ("repository", "sha", "path")}, ["repository", "sha", "path"]),
            tool("search_code", "Search local ExetRouter tracked sources (literal query).",
                 {"query": {"type": "string"}}, ["query"]),
            {"type": "function", "name": "submit_decision", "strict": False, "description":
             "Finish with a concise evidence-backed verdict, not hidden chain of thought.",
             "parameters": {**SCHEMA, "properties": {**SCHEMA["properties"],
                 "decision": {"type": "string", "enum": sorted(DECISIONS[phase])}}}},
        ]
        if phase == "verify":
            tools.append(tool("submit_preliminary", "Freeze independent verdict and reveal triage.",
                              {"summary": {"type": "string"}}, ["summary"]))
        if phase == "implement":
            tools.extend([
                tool("write_file", "Replace an allowed source file. No deletes, symlinks or automation edits.",
                     {"path": {"type": "string"}, "content": {"type": "string"}}, ["path", "content"]),
                tool("run_check", "Run a named isolated check: fmt, clippy, tests, publication.",
                     {"name": {"type": "string", "enum": ["fmt", "clippy", "tests", "publication"]}}, ["name"]),
            ])
        revealed = False
        for _ in range(24 if phase == "implement" else 8):
            if self.calls >= 40 or self.tokens >= 600000 or not self.usage_known:
                raise Stop("model_budget_or_unknown_usage")
            self.calls += 1
            response = self.call(self.base + "/responses", self.token, "POST", {
                "model": MODEL, "reasoning": {"effort": "medium" if phase == "triage" else "xhigh"},
                "store": False, "stream": False, "input": history, "tools": tools,
                "include": ["reasoning.encrypted_content"], "parallel_tool_calls": False,
                "tool_choice": "required"})
            if response.get("status") != "completed":
                raise Stop("model_incomplete")
            usage = response.get("usage") or {}
            counters = [usage.get("input_tokens"), usage.get("output_tokens")]
            self.usage_known = all(type(n) is int and n >= 0 for n in counters)
            if self.usage_known:
                self.tokens += sum(counters)
            outputs = response.get("output", [])
            history.extend(outputs)  # Preserve opaque context and exact call IDs.
            calls = [i for i in outputs if i.get("type") == "function_call"]
            if len(calls) != 1:
                raise Stop("expected_one_tool_call")
            item = calls[0]
            try:
                args = json.loads(item["arguments"])
            except (KeyError, ValueError):
                raise Stop("invalid_tool_arguments") from None
            name = item["name"]
            if name == "submit_decision":
                if phase == "verify" and not revealed:
                    raise Stop("independent_verdict_missing")
                result = decision(phase, args)
                sandbox.validate_citations(result)
                return result
            if name == "submit_preliminary" and phase == "verify" and not revealed:
                if not isinstance(args.get("summary"), str) or not args["summary"]:
                    raise Stop("invalid_preliminary")
                revealed = True
                result = {"triage": triage}
            else:
                result = sandbox.dispatch(name, args, editable=phase == "implement")
            history.append({"type": "function_call_output", "call_id": item["call_id"],
                            "output": json.dumps(result)})
        raise Stop("phase_budget_exceeded")
