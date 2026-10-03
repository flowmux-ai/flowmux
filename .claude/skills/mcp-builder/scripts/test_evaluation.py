# SPDX-License-Identifier: GPL-3.0-or-later
"""Offline regression checks; no API credentials or MCP server required."""
import importlib.util
import json
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import patch


class Content:
    def model_dump(self, **kwargs):
        return {"type": "text", "text": "tool output"}


class AgentLoopTests(unittest.IsolatedAsyncioTestCase):
    async def test_all_tool_calls_receive_serialized_results(self):
        spec = importlib.util.spec_from_file_location(
            "evaluation_under_test", Path(__file__).with_name("evaluation.py")
        )
        module = importlib.util.module_from_spec(spec)
        with patch.dict("sys.modules", {
            "anthropic": SimpleNamespace(Anthropic=object),
            "connections": SimpleNamespace(create_connection=None),
        }):
            spec.loader.exec_module(module)
        calls = []
        responses = []

        async def call_tool(name, arguments):
            calls.append(name)
            if name == "failing":
                raise RuntimeError("fixture failure")
            return [Content()]

        def create(**kwargs):
            if not responses:
                responses.append(True)
                return SimpleNamespace(stop_reason="tool_use", content=[
                    SimpleNamespace(type="tool_use", id="1", name="first", input={}),
                    SimpleNamespace(type="tool_use", id="2", name="failing", input={}),
                    SimpleNamespace(type="tool_use", id="3", name="last", input={}),
                ])
            results = kwargs["messages"][-1]["content"]
            self.assertEqual([r["tool_use_id"] for r in results], ["1", "2", "3"])
            self.assertEqual(json.loads(results[0]["content"]), [Content().model_dump()])
            self.assertTrue(results[1]["is_error"])
            self.assertEqual(json.loads(results[2]["content"]), [Content().model_dump()])
            return SimpleNamespace(stop_reason="end_turn", content=[
                SimpleNamespace(type="text", text="<response>OK</response>")
            ])

        answer, metrics = await module.agent_loop(
            SimpleNamespace(messages=SimpleNamespace(create=create)),
            "fixture", "question", [], SimpleNamespace(call_tool=call_tool),
        )
        self.assertEqual(calls, ["first", "failing", "last"])
        self.assertEqual(answer, "<response>OK</response>")
        self.assertEqual({k: v["count"] for k, v in metrics.items()},
                         {"first": 1, "failing": 1, "last": 1})


if __name__ == "__main__":
    unittest.main()
