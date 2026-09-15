#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Run with python3 scripts/test-license-notices.py."""

from pathlib import Path
import runpy
import unittest
from unittest.mock import patch

generator = runpy.run_path(str(Path(__file__).with_name("generate-third-party-licenses.py")))


def license_for(name, text, identifier="MIT"):
    return {"id": identifier, "name": identifier, "text": text,
            "used_by": [{"crate": {"name": name, "version": "1.0"}}]}


class NoticeTests(unittest.TestCase):
    def test_wrong_tool_version_stops_before_generating_notices(self):
        with patch("subprocess.check_output", return_value="cargo-about 0.9.1\n") as command:
            with self.assertRaisesRegex(SystemExit, "Expected cargo-about 0.9.2"):
                generator["main"]()
        self.assertEqual(command.call_count, 1)

    def test_shared_terms_keep_each_component_and_copyright(self):
        terms = "Permission is hereby granted to use.\nNo warranty."
        entries = [license_for("one", "Copyright Alice\n" + terms),
                   license_for("two", "Copyright Bob\n" + terms.replace("\n", "  \n"))]
        groups = list(generator["group_licenses"](entries))
        self.assertEqual(len(groups), 1)
        rendered = generator["render"](entries)
        for credit in ["Copyright Alice", "Copyright Bob", "one 1.0", "two 1.0"]:
            self.assertIn(credit, rendered)
        self.assertEqual(rendered.count("Permission is hereby granted"), 1)

    def test_additional_terms_and_unknown_formats_are_preserved(self):
        text = "Copyright Alice\nPermission is hereby granted to use."
        entries = [license_for("one", text), license_for("two", text + " Extra condition."),
                   license_for("three", "Custom terms\nCopyright Carol", "custom")]
        self.assertEqual(len(list(generator["group_licenses"](entries))), 3)
        rendered = generator["render"](entries)
        self.assertIn("Extra condition.", rendered)
        self.assertIn("Custom terms\nCopyright Carol", rendered)

    def test_embedded_markdown_does_not_end_the_license_block(self):
        text = "Upstream notice\n~~~~\nAdditional terms"
        self.assertEqual(generator["block"](text), f"~~~~~text\n{text}\n~~~~~")


if __name__ == "__main__":
    unittest.main()
