"""Unit tests for the herdr-agency control tool.

Run: python3 -m unittest discover -s agency/tests
These tests cover the server-free surface: frontmatter parsing, roster
compilation, validation, and offline routing. They do not require a herdr server.
"""
import importlib.util
import json
import sys
import tempfile
import unittest
from importlib.machinery import SourceFileLoader
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TOOL = REPO / "bin" / "herdr-agency"
TEMPLATES = REPO / "templates"

# The tool has no .py extension, so load it explicitly as a source file.
# Register in sys.modules before exec so dataclass introspection works.
loader = SourceFileLoader("herdr_agency", str(TOOL))
spec = importlib.util.spec_from_loader("herdr_agency", loader)
agency_mod = importlib.util.module_from_spec(spec)
sys.modules["herdr_agency"] = agency_mod
loader.exec_module(agency_mod)


class FrontmatterTests(unittest.TestCase):
    def test_scalars_lists_and_body(self):
        text = (
            "---\n"
            "name: backend\n"
            "complexity: high\n"
            "args: [\"--model\", \"opus\"]\n"
            "tags: [api, rust]  # routing keywords\n"
            "skills: []\n"
            "---\n"
            "You are the backend engineer.\n"
        )
        fm, body = agency_mod.parse_frontmatter(text)
        self.assertEqual(fm["name"], "backend")
        self.assertEqual(fm["complexity"], "high")
        self.assertEqual(fm["args"], ["--model", "opus"])
        self.assertEqual(fm["tags"], ["api", "rust"])
        self.assertEqual(fm["skills"], [])
        self.assertEqual(body, "You are the backend engineer.")

    def test_no_frontmatter(self):
        fm, body = agency_mod.parse_frontmatter("just a body")
        self.assertEqual(fm, {})
        self.assertEqual(body, "just a body")


class LoadAndValidateTests(unittest.TestCase):
    def _load_templates(self):
        with tempfile.TemporaryDirectory() as tmp:
            dest = Path(tmp) / "agency"
            import shutil
            shutil.copytree(TEMPLATES, dest)
            return agency_mod.load_agency(dest), dest

    def test_templates_load_and_validate(self):
        agency, _ = self._load_templates()
        self.assertEqual(agency.orchestrator, "manager")
        self.assertIn("backend", agency.agents)
        self.assertTrue(agency.agents["manager"].is_orchestrator)
        self.assertEqual(agency.agents["backend"].argv(), ["claude", "--model", "opus"])
        self.assertEqual(agency_mod.validate_agency(agency), [])

    def test_missing_orchestrator_is_error(self):
        agency, _ = self._load_templates()
        agency.orchestrator = "nobody"
        errors = agency_mod.validate_agency(agency)
        self.assertTrue(any("orchestrator" in e for e in errors))

    def test_default_command_applies(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp) / "agency"
            (root / "agents").mkdir(parents=True)
            (root / "agency.toml").write_text(
                '[agency]\norchestrator = "m"\ndefault_command = "codex"\n'
            )
            (root / "agents" / "m.md").write_text(
                "---\nname: m\nrole: boss\n---\nbody\n"
            )
            agency = agency_mod.load_agency(root)
            self.assertEqual(agency.agents["m"].command, "codex")


class RosterAndPlanTests(unittest.TestCase):
    def _agency(self):
        import shutil
        tmp = tempfile.mkdtemp()
        dest = Path(tmp) / "agency"
        shutil.copytree(TEMPLATES, dest)
        return agency_mod.load_agency(dest)

    def test_roster_shape(self):
        roster = agency_mod.compile_roster(self._agency())
        self.assertEqual(roster["orchestrator"], "manager")
        names = {a["name"] for a in roster["agents"]}
        self.assertEqual(names, {"manager", "backend", "frontend", "researcher"})

    def test_plan_routes_by_tags(self):
        result = agency_mod.plan_task(self._agency(), "build the frontend ui form")
        top = result["candidates"][0]["name"]
        self.assertEqual(top, "frontend")

    def test_plan_routes_research(self):
        result = agency_mod.plan_task(self._agency(), "research the docs and search the code")
        top = result["candidates"][0]["name"]
        self.assertEqual(top, "researcher")


class DispatchCursorTests(unittest.TestCase):
    def _agency(self):
        import shutil
        tmp = tempfile.mkdtemp()
        dest = Path(tmp) / "agency"
        shutil.copytree(TEMPLATES, dest)
        return agency_mod.load_agency(dest)

    def test_pending_tracks_cursor_and_is_idempotent(self):
        agency = self._agency()
        self.assertEqual(agency_mod.pending_requests(agency), [])
        agency_mod.enqueue_request(agency, "first")
        agency_mod.enqueue_request(agency, "second")
        pending = agency_mod.pending_requests(agency)
        self.assertEqual([e["request"] for _, e in pending], ["first", "second"])
        # Mark both dispatched; nothing pending afterwards.
        agency_mod._set_dispatch_cursor(agency, 2)
        self.assertEqual(agency_mod.pending_requests(agency), [])
        # A later request is the only new pending one.
        agency_mod.enqueue_request(agency, "third")
        pending = agency_mod.pending_requests(agency)
        self.assertEqual([e["request"] for _, e in pending], ["third"])

    def test_dispatch_without_orchestrator_leaves_pending(self):
        agency = self._agency()
        agency_mod.enqueue_request(agency, "work")
        # No runtime.json (and no herdr server): nothing delivered, still pending.
        self.assertEqual(agency_mod.dispatch_pending(agency), [])
        self.assertEqual(len(agency_mod.pending_requests(agency)), 1)


class McpTests(unittest.TestCase):
    def _agency(self):
        import shutil
        tmp = tempfile.mkdtemp()
        dest = Path(tmp) / "agency"
        shutil.copytree(TEMPLATES, dest)
        return agency_mod.load_agency(dest)

    def test_initialize(self):
        agency = self._agency()
        resp = agency_mod.mcp_handle(agency, {"jsonrpc": "2.0", "id": 1, "method": "initialize"})
        self.assertEqual(resp["id"], 1)
        self.assertEqual(resp["result"]["protocolVersion"], agency_mod.MCP_PROTOCOL_VERSION)
        self.assertIn("tools", resp["result"]["capabilities"])

    def test_notification_returns_none(self):
        agency = self._agency()
        resp = agency_mod.mcp_handle(
            agency, {"jsonrpc": "2.0", "method": "notifications/initialized"}
        )
        self.assertIsNone(resp)

    def test_tools_list_exposes_submit_task(self):
        agency = self._agency()
        resp = agency_mod.mcp_handle(agency, {"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
        names = {t["name"] for t in resp["result"]["tools"]}
        self.assertEqual(names, {"submit_task", "agency_roster", "agency_status"})

    def test_tools_call_submit_task_enqueues(self):
        agency = self._agency()
        resp = agency_mod.mcp_handle(
            agency,
            {
                "jsonrpc": "2.0",
                "id": 3,
                "method": "tools/call",
                "params": {"name": "submit_task", "arguments": {"request": "ship it"}},
            },
        )
        payload = json.loads(resp["result"]["content"][0]["text"])
        self.assertTrue(payload["queued"])
        self.assertTrue(payload["id"].startswith("task-"))
        self.assertEqual(len(agency_mod._read_inbox(agency)), 1)

    def test_tools_call_empty_request_errors(self):
        agency = self._agency()
        resp = agency_mod.mcp_handle(
            agency,
            {
                "jsonrpc": "2.0",
                "id": 4,
                "method": "tools/call",
                "params": {"name": "submit_task", "arguments": {"request": "  "}},
            },
        )
        self.assertIn("error", resp)

    def test_roster_tool_returns_agents(self):
        agency = self._agency()
        text = agency_mod.mcp_call_tool(agency, "agency_roster", {})
        roster = json.loads(text)
        self.assertEqual(roster["orchestrator"], "manager")


if __name__ == "__main__":
    unittest.main()
