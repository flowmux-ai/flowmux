# SPDX-License-Identifier: GPL-3.0-or-later
"""The Agent Host protocol version of this checkout, for fixtures that speak it directly."""

import re
from pathlib import Path


def protocol_version():
    source = (Path(__file__).resolve().parents[1] / "crates/flowmux-ipc/src/agent_host.rs").read_text()
    match = re.search(r"^pub const PROTOCOL_VERSION: u32 = (\d+);", source, re.M)
    assert match, "Agent Host PROTOCOL_VERSION not found"
    return int(match.group(1))


PROTOCOL_VERSION = protocol_version()
