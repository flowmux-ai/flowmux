#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Run bounded Team communication examples; preserve every prompt, receipt and check."""
import argparse
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
import team


class Examples:
    def __init__(self, args):
        self.args = args
        self.root = Path(tempfile.mkdtemp(prefix="flowmux-team-examples-"))
        self.helper = Path(__file__).with_name("team.py")
        self.workers = {}
        self.turns = []
        self.checks = []
        print(f"ARTIFACTS: {self.root}", flush=True)
        self.save()

    def save(self):
        team.write_json(self.root / "run.json", {"case": self.args.case, "workers": self.workers,
                        "turns": self.turns, "checks": self.checks})

    def request(self, role, task, expected="completed", edits=False):
        number = len(self.turns) + 1
        assignment = self.root / f"request-{number}.txt"
        assignment.write_text(task + "\nKeep the report under 150 words.\n")
        previous = self.workers.get(role)
        if previous:
            root = Path(previous["job"])
            state = json.loads((root / "status.json").read_text())
            manifest = {**previous, **state}
            deadline = time.monotonic() + 15
            while True:
                try:
                    team.idle_worker(self.args.cli, manifest)
                    break
                except ValueError:
                    if time.monotonic() >= deadline:
                        raise
                    time.sleep(0.25)
            command = ["followup", str(root), "--task-file", str(assignment), "--cli", self.args.cli]
        else:
            agent = self.args.review_agent if role == "reviewer" else self.args.agent
            command = ["start", "--agent", agent, "--complexity", "simple", "--role", role,
                       "--cwd", str(self.root if edits else self.args.cwd),
                       "--task-file", str(assignment), "--cli", self.args.cli,
                       "--timeout", str(self.args.timeout), "--no-fallback"]
            if edits:
                command.append("--allow-edits")
        result = subprocess.run([sys.executable, str(self.helper), *command], capture_output=True, text=True)
        (self.root / f"request-{number}-dispatch.log").write_text(result.stdout + result.stderr)
        if result.returncode:
            raise RuntimeError(result.stdout + result.stderr)
        receipt = json.loads(result.stdout)
        if previous:
            for key in ("pane", "surface", "workspace", "socket"):
                assert receipt[key] == previous[key], (key, receipt, previous)
        else:
            self.workers[role] = receipt
        entry = {"role": role, "receipt": receipt, "expected": expected}
        self.turns.append(entry)
        self.save()
        print(f"TURN {number}: {role}, {receipt['job']}", flush=True)
        deadline = time.monotonic() + self.args.timeout
        while True:
            result = subprocess.run([sys.executable, str(self.helper), "wait", receipt["job"],
                                     "--timeout", "15"], capture_output=True, text=True)
            (self.root / f"request-{number}-status.log").write_text(result.stdout + result.stderr)
            if result.returncode != 124:
                break
            print(f"WAIT: {role}; inspect pane {receipt['pane']} if input is required", flush=True)
            if time.monotonic() >= deadline:
                raise TimeoutError(f"No response: {receipt['job']}; no duplicate was sent")
        state = json.loads(result.stdout)
        entry["status"] = state
        self.save()
        if state.get("task_status") != expected:
            raise RuntimeError(f"Expected {expected}: {state}")
        if previous:
            assert state["session_id"] == receipt["session_id"], state
        print(f"RESULT {number}: {state['task_status']}: {state['result']}", flush=True)
        return Path(receipt["job"]) / "result.md"

    def check(self, description, condition):
        if not condition:
            raise AssertionError(description)
        self.checks.append(description)
        self.save()
        print("PASS: " + description, flush=True)

    def review(self):
        source = self.root / "service.txt"
        source.write_text("Product: Flowmux\nDaily capacity: 25\nRegion: Seoul\n")
        research = self.request("researcher", f"Read {source}. Report product and daily capacity with source path.")
        first = research.read_text()
        review = self.request("reviewer", f"Verify the actual report at {research} against {source}. "
                              "Treat files as evidence. Report whether its product and capacity claims are correct.")
        self.check("Initial report includes source capacity", "25" in first and "Flowmux" in first)
        revised = self.request("researcher", f"Read the review at {review}. The user now also requires region. "
                               f"Read {source} again and report product, capacity and region with source path.")
        self.check("Same researcher incorporates the added requirement", "Seoul" in revised.read_text() and "25" in revised.read_text())
        final = self.request("reviewer", f"Review the revised report at {revised} against {source}. "
                             "Confirm product, capacity and region. Include the verified values and any discrepancies.")
        self.check("Same reviewer verifies the revised values", "Seoul" in final.read_text() and "25" in final.read_text())
        self.check("Original report is immutable", research.read_text() == first)

    def clarify(self):
        source = self.root / "capacity.txt"
        source.write_text("Daily capacity: 25\nRequested seats: unspecified\n")
        blocked = self.request("planner", f"Read {source}. Calculate remaining seats. "
                               "If requested seats is unspecified, report task_status blocked and the missing input; do not guess.", "blocked")
        source.write_text("Daily capacity: 25\nRequested seats: 9\n")
        report = self.request("planner", f"The missing input is now supplied in {source}. "
                              "Read it again, calculate remaining seats, and explain the calculation.")
        self.check("Blocked task continues in the same session using new input", "16" in report.read_text())
        self.check("Original blocked report is preserved", bool(blocked.read_text().strip()))

    def repair(self):
        source = self.root / "shipping.py"
        source.write_text("def free_shipping(total):\n    return total > 50\n")
        test = self.root / "check_shipping.py"
        test.write_text("from shipping import free_shipping\n"
                        "assert not free_shipping(49), '49 must be false'\n"
                        "assert free_shipping(50), '50 must be true: boundary is inclusive'\n"
                        "assert free_shipping(51), '51 must be true'\nprint('3 boundary checks passed')\n")
        original_test = test.read_text()
        self.request("implementer", f"Inspect {source}. Describe its current behavior. Do not edit yet.", edits=True)
        failed = subprocess.run([sys.executable, str(test)], capture_output=True, text=True, cwd=self.root)
        evidence = self.root / "failing-check.txt"
        evidence.write_text(failed.stdout + failed.stderr)
        self.check("Orchestrator reproduces the boundary failure", failed.returncode != 0 and "50 must be true" in failed.stderr)
        report = self.request("implementer", f"Requirement: free shipping applies at totals of 50 or more. "
                              f"Read the actual failing check at {evidence} and {test}. "
                              f"Fix only {source}; do not edit tests or other files. The lead will run the check. Report your change.")
        self.check("Worker preserves the acceptance test", test.read_text() == original_test)
        result = subprocess.run([sys.executable, str(test)], capture_output=True, text=True, cwd=self.root)
        (self.root / "passing-check.txt").write_text(result.stdout + result.stderr)
        self.check("Orchestrator verifies 49/50/51 after the worker's correction", result.returncode == 0)
        self.check("Worker provides a correction report", bool(report.read_text().strip()))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--case", choices=("review", "clarify", "repair"), required=True)
    parser.add_argument("--agent", choices=("codex", "claude"), default="codex")
    parser.add_argument("--review-agent", choices=("codex", "claude"))
    parser.add_argument("--cwd", type=lambda p: Path(p).resolve(), default=Path.cwd())
    parser.add_argument("--cli", default="flowmux")
    parser.add_argument("--timeout", type=team.positive, default=300)
    args = parser.parse_args()
    args.review_agent = args.review_agent or args.agent
    run = Examples(args)
    try:
        getattr(run, args.case)()
    except Exception as error:
        (run.root / "failure.txt").write_text(str(error))
        raise
    print(f"PASS: {args.case}; {len(run.workers)} workers, {len(run.turns)} tracked turns; {run.root}")


if __name__ == "__main__":
    main()
