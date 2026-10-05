"""Generated L0 fixtures, never requires UM, network, drivers or a game."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("game_experiment", REPO / "experiments/05-game-aware-packing/run_benchmark.py")
EXPERIMENT = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(EXPERIMENT)


class FixtureTests(unittest.TestCase):
    def test_generation_reproducible_and_keeps_stable_records(self):
        with tempfile.TemporaryDirectory() as a, tempfile.TemporaryDirectory() as b:
            aa = EXPERIMENT.generate(Path(a), 2)
            bb = EXPERIMENT.generate(Path(b), 2)
            self.assertEqual([EXPERIMENT.tree_identity(p) for p in aa], [EXPERIMENT.tree_identity(p) for p in bb])
            self.assertNotEqual(EXPERIMENT.tree_identity(aa[0]), EXPERIMENT.tree_identity(aa[1]))

    def test_summary_uses_every_trial_and_missing_stays_null(self):
        self.assertEqual(EXPERIMENT.stats([1, 10, 100])["median"], 10)
        self.assertEqual(EXPERIMENT.stats([1, 10, 100])["trials"], 3)
        self.assertIsNone(EXPERIMENT.stats([1, None, 100]))


@unittest.skipUnless(os.environ.get("PLAYSPARSE_GAME_TEST_BINARY"), "set PLAYSPARSE_GAME_TEST_BINARY after locked release build")
class CliTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.work = Path(self.temp.name)
        self.source = self.work / "fixture Unicode игра ' $(literal)"
        self.source.mkdir()
        roots = EXPERIMENT.generate(self.source, 2)
        self.source = roots[0]
        self.before = EXPERIMENT.tree_identity(self.source)
        self.binary = str(Path(os.environ["PLAYSPARSE_GAME_TEST_BINARY"]).resolve())

    def run_cli(self, *args, success=True):
        result = subprocess.run([self.binary, *map(str, args)], capture_output=True, timeout=30)
        self.assertEqual(result.returncode == 0, success, result.stderr.decode(errors="replace"))
        return json.loads(result.stdout) if success else result

    def profile(self, container=False):
        p, plan = self.work / "profile.json", self.work / "plan.json"
        args = ["inspect-game", self.source, "--scanner", "generic", "--output", p, "--plan-output", plan]
        if container:
            args += ["--container-aware"]
        self.run_cli(*args)
        return p, plan

    def test_generic_absent_fallback_and_byte_identity(self):
        result = self.run_cli("inspect-game", self.source, "--scanner-program", self.work / "absent")
        self.assertEqual(result["profile"]["scanner_status"], "absent-fallback")
        self.run_cli("inspect-game", self.source, "--scanner", "universal-modder", "--scanner-program", self.work / "absent", success=False)
        _, plan = self.profile()
        generic, aware = self.work / "generic", self.work / "aware"
        self.run_cli("pack", self.source, generic)
        self.run_cli("pack", self.source, aware, "--plan", plan)
        for name in ["manifest.json", "index/objects.idx", "packs/pack-0000.psp"]:
            self.assertEqual((generic / name).read_bytes(), (aware / name).read_bytes())
        self.assertEqual(EXPERIMENT.tree_identity(self.source), self.before)

    def test_zip_original_bytes_and_plan_tampering(self):
        _, plan = self.profile(container=True)
        value = json.loads(plan.read_text())
        self.assertTrue(any(f["chunk_strategy"] == "zip-records-cdc" for f in value["files"]))
        store = self.work / "aware"
        self.run_cli("pack", self.source, store, "--plan", plan)
        self.run_cli("verify", store)
        for p in self.source.iterdir():
            actual = subprocess.run([self.binary, "read", str(store), p.name, "0", str(p.stat().st_size)], capture_output=True, timeout=30)
            self.assertEqual(actual.returncode, 0)
            self.assertEqual(actual.stdout, p.read_bytes())
        value["files"][0]["boundaries"][0] += 1
        plan.write_text(json.dumps(value))
        self.run_cli("pack", self.source, self.work / "new-parent/bad-store", "--plan", plan, success=False)
        self.assertFalse((self.work / "new-parent").exists())
        self.assertEqual(EXPERIMENT.tree_identity(self.source), self.before)

    def test_profile_version_paths_source_mismatch_and_nested_output(self):
        p, _ = self.profile()
        original = json.loads(p.read_text())
        for field, value in [("schema_version", 99), ("files", [{**original["files"][0], "path": "../outside"}])]:
            bad = {**original, field: value}
            p.write_text(json.dumps(bad))
            self.run_cli("pack", self.source, self.work / "bad", "--profile", p, success=False)
            self.assertFalse((self.work / "bad").exists())
        p.write_text(json.dumps(original))
        self.run_cli("inspect-game", self.source, "--output", self.source / "new/profile.json", success=False)
        self.assertFalse((self.source / "new").exists())
        (self.source / "raw.dat").write_bytes(b"different")
        self.run_cli("pack", self.source, self.work / "bad", "--profile", p, success=False)
        self.assertFalse((self.work / "bad").exists())

    def test_analyze_profile_retains_generic_baseline(self):
        p, _ = self.profile()
        result = self.run_cli("analyze", self.source, "--profile", p)
        self.assertIn("fixed_chunks", result)
        self.assertIn("cdc_chunks", result)
        self.assertTrue(result["game_aware"]["experimental"])
        self.assertIsNone(result["game_aware"]["already_compressed_bytes"])
        self.assertEqual(EXPERIMENT.tree_identity(self.source), self.before)


if __name__ == "__main__":
    unittest.main()
