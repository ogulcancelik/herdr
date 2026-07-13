from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from scripts.config_reference_check import (
    Model,
    StructField,
    check,
    collect_entries,
    collect_keys,
    parse_file,
    parse_model,
)


SAMPLE_MODEL = """
#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct Config {
    pub onboarding: Option<bool>,
    pub ui: UiConfig,
    pub keys: KeysConfig,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct UiConfig {
    /// Sidebar width in columns. Default: 26.
    pub sidebar_width: u16,
    /// Host cursor policy. Default: auto.
    pub host_cursor: HostCursorModeConfig,
    #[serde(rename = "accent_color")]
    pub accent: String,
    #[serde(skip)]
    pub internal_cache: usize,
}

#[derive(Debug, Deserialize)]
#[serde(default)]
pub struct KeysConfig {
    /// Prefix key. Default: "ctrl+b".
    pub prefix: String,
    pub zoom: BindingConfig,
    /// Prefix-mode custom command bindings.
    pub command: Vec<CommandKeybindConfig>,
    pub(crate) user_fields: BTreeSet<&'static str>,
}

#[derive(Debug, Deserialize)]
pub struct CommandKeybindConfig {
    pub key: String,
    pub command: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostCursorModeConfig {
    Auto,
    NativeCursor,
    Drawn,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum BindingConfig {
    One(String),
    Many(Vec<String>),
}
"""


def sample_model() -> Model:
    model = Model()
    parse_file(SAMPLE_MODEL, model)
    return model


class CollectKeysTests(unittest.TestCase):
    def test_walks_nested_structs_into_dotted_keys(self) -> None:
        keys = collect_keys(sample_model())

        self.assertIn("onboarding", keys)
        self.assertIn("ui.sidebar_width", keys)
        self.assertIn("keys.prefix", keys)
        self.assertIn("keys.zoom", keys)

    def test_serde_rename_wins_over_field_name(self) -> None:
        keys = collect_keys(sample_model())

        self.assertIn("ui.accent_color", keys)
        self.assertNotIn("ui.accent", keys)

    def test_skips_serde_skip_and_private_fields(self) -> None:
        keys = collect_keys(sample_model())

        self.assertNotIn("ui.internal_cache", keys)
        self.assertNotIn("keys.user_fields", keys)

    def test_skips_listed_vec_of_struct_subtrees(self) -> None:
        keys = collect_keys(sample_model())

        self.assertNotIn("keys.command", keys)
        self.assertNotIn("keys.command.key", keys)

    def test_unlisted_vec_of_struct_subtree_is_an_error(self) -> None:
        model = sample_model()
        parse_file(
            "pub struct ExtraConfig {\n"
            "    pub items: Vec<CommandKeybindConfig>,\n"
            "}\n",
            model,
        )
        model.structs["Config"].append(
            StructField(name="extra", rust_type="ExtraConfig", doc="")
        )

        with self.assertRaises(ValueError) as raised:
            collect_keys(model)

        self.assertIn("extra.items", str(raised.exception))
        self.assertIn("SKIPPED_SUBTREES", str(raised.exception))

    def test_enum_values_respect_rename_all_and_untagged_enums_have_none(self) -> None:
        entries = {entry["key"]: entry for entry in collect_entries(sample_model())}

        self.assertEqual(
            entries["ui.host_cursor"]["values"], ["auto", "native-cursor", "drawn"]
        )
        self.assertNotIn("values", entries["keys.zoom"])


class CheckTests(unittest.TestCase):
    def run_check(
        self,
        documented_keys: list[str],
        *,
        value_overrides: dict[str, list[str]] | None = None,
    ) -> list[str]:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            model_root = root / "config"
            model_root.mkdir()
            (model_root / "model.rs").write_text(SAMPLE_MODEL, encoding="utf-8")

            model_entries = {entry["key"]: entry for entry in collect_entries(sample_model())}
            entries = []
            for key in documented_keys:
                entry = {"key": key}
                if key in model_entries and "values" in model_entries[key]:
                    entry["values"] = model_entries[key]["values"]
                if value_overrides and key in value_overrides:
                    entry["values"] = value_overrides[key]
                entries.append(entry)

            reference = root / "config-reference.json"
            reference.write_text(
                json.dumps(
                    {
                        "sections": [
                            {
                                "id": "all",
                                "title": "All",
                                "keys": entries,
                            }
                        ]
                    }
                ),
                encoding="utf-8",
            )
            return check(model_root, reference)

    def all_keys(self) -> list[str]:
        return sorted(collect_keys(sample_model()))

    def test_in_sync_reference_passes(self) -> None:
        self.assertEqual(self.run_check(self.all_keys()), [])

    def test_missing_key_is_named(self) -> None:
        documented = [key for key in self.all_keys() if key != "ui.sidebar_width"]

        errors = self.run_check(documented)

        self.assertEqual(len(errors), 1)
        self.assertIn("ui.sidebar_width", errors[0])
        self.assertIn("missing", errors[0])

    def test_stale_key_is_named(self) -> None:
        errors = self.run_check(self.all_keys() + ["ui.removed_option"])

        self.assertEqual(len(errors), 1)
        self.assertIn("ui.removed_option", errors[0])
        self.assertIn("not in src/config", errors[0])

    def test_swapped_key_fails_despite_equal_count(self) -> None:
        documented = [
            "ui.renamed_option" if key == "ui.sidebar_width" else key
            for key in self.all_keys()
        ]

        errors = self.run_check(documented)

        self.assertEqual(len(errors), 2)

    def test_duplicate_key_is_rejected(self) -> None:
        errors = self.run_check(self.all_keys() + ["ui.sidebar_width"])

        self.assertEqual(len(errors), 1)
        self.assertIn("duplicated", errors[0])

    def test_changed_enum_values_are_rejected(self) -> None:
        errors = self.run_check(
            self.all_keys(),
            value_overrides={"ui.host_cursor": ["auto", "native"]},
        )

        self.assertEqual(len(errors), 1)
        self.assertIn("ui.host_cursor", errors[0])
        self.assertIn("allowed values", errors[0])


class RealModelTests(unittest.TestCase):
    def test_real_config_model_parses_and_yields_keys(self) -> None:
        model = parse_model(sorted(Path("src/config").glob("*.rs")))
        keys = collect_keys(model)

        self.assertGreater(len(keys), 100)
        self.assertIn("keys.prefix", keys)
        self.assertIn("ui.sound.agents.claude", keys)
        self.assertNotIn("keys.command", keys)

    def test_preview_reference_matches_real_config_model(self) -> None:
        self.assertEqual(
            check(
                Path("src/config"),
                Path("docs/next/website/src/data/config-reference.json"),
            ),
            [],
        )


if __name__ == "__main__":
    unittest.main()
