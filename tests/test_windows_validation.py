import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import struct
import subprocess
import sys
import tempfile
import unittest
from unittest import mock

TOOLS = Path(__file__).resolve().parents[1] / "tools"
sys.path.insert(0, str(TOOLS))
import validation_common as common
import windows_support as support


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, TOOLS / filename)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


comparison = load("windows_comparison_test", "windows-comparison.py")
validation = load("windows_validation_test", "windows-validation.py")


class PortableWindowsTests(unittest.TestCase):
    def test_stream_allocation_includes_compressed_backing_data(self):
        names = ["::$DATA", ":WofCompressedData:$DATA"]
        records = []
        for index, name in enumerate(names):
            encoded = name.encode("utf-16-le")
            length = (24 + len(encoded) + 7) // 8 * 8
            records.append(struct.pack("<IIqq", length if index == 0 else 0,
                                       len(encoded), 10000 if index == 0 else 1234,
                                       0 if index == 0 else 4096) + encoded
                           + b"\0" * (length - 24 - len(encoded)))
        streams = support.parse_streams(b"".join(records))
        self.assertEqual([row["name"] for row in streams], names)
        self.assertEqual(sum(row["allocated_bytes"] for row in streams), 4096)
        for bad in [b"", struct.pack("<IIqq", 1, 0, 0, 0),
                    struct.pack("<IIqq", 0, 3, 0, 0) + b"xxx",
                    struct.pack("<IIqq", 0, 0, -1, 0)]:
            with self.assertRaises(ValueError):
                support.parse_streams(bad)

    def test_no_vm_hint_is_not_physical_evidence(self):
        environment = {"is_windows": True, "windows_product_type": 1,
                       "virtual_machine_detected": False}
        self.assertEqual(support.physical_gate(environment, False)["status"], "BLOCKED")
        self.assertEqual(support.physical_gate(environment, True)["status"], "NOT RUN")
        for hint in [True, None]:
            environment["virtual_machine_detected"] = hint
            self.assertEqual(support.physical_gate(environment, True)["status"], "BLOCKED")
        environment.update(virtual_machine_detected=False, windows_product_type=3)
        self.assertEqual(support.physical_gate(environment, True)["status"], "BLOCKED")

    def test_disposable_copy_is_independent_and_original_is_unchanged(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw); source = root / "owned source"; source.mkdir()
            (source / "empty directory").mkdir()
            (source / "data.bin").write_bytes(b"payload" * 1000)
            before = support.fingerprint(source)
            copy = root / "disposable WOF copy"
            comparison.copy_disposable(source, copy)
            self.assertEqual(support.content_identity(before), support.content_identity(support.fingerprint(copy)))
            (copy / "data.bin").write_bytes(b"copy only")
            self.assertEqual(before["tree_sha256"], support.fingerprint(source)["tree_sha256"])
            with self.assertRaises(ValueError):
                comparison.copy_disposable(source, source / "bad")

    def test_work_checks_every_source_before_mkdir(self):
        with tempfile.TemporaryDirectory() as raw:
            source = Path(raw) / "wof source"; source.mkdir()
            work = source / "new evidence"
            with self.assertRaises(common.PrerequisiteMissing):
                common.safe_new_work(work, sources=[None, source])
            self.assertFalse(work.exists())

    def test_wof_is_verified_by_provider_and_algorithm(self):
        class API:
            def __init__(self, provider, algorithm): self.provider, self.algorithm = provider, algorithm
            def wof_info(self, path):
                return {"externally_backed": bool(self.provider), "provider": self.provider, "algorithm": self.algorithm}
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw); (root / "file").write_bytes(b"data")
            with self.assertRaises(support.WindowsPrerequisite):
                comparison.verified_wof(root, API(0, None), "XPRESS4K")
            with self.assertRaises(RuntimeError):
                comparison.verified_wof(root, API(2, 1), "XPRESS4K")
            self.assertEqual(comparison.verified_wof(root, API(2, 0), "XPRESS4K")["status"], "VERIFIED")
        with self.assertRaises(ValueError):
            comparison.compact_command("copy", "XPRESS4K & unsafe")

    def test_identical_query_plan_and_byte_checks(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw); (root / "file with spaces").write_bytes(bytes(range(256)) * 1000)
            snapshot = support.fingerprint(root)
            queries = support.query_plan(snapshot, count=9)
            self.assertEqual(queries, support.query_plan(snapshot, count=9))
            first, expected = support.read_workload(root, queries)
            second, _ = support.read_workload(root, queries, expected)
            self.assertEqual(first["query_digest_sha256"], second["query_digest_sha256"])
            self.assertLessEqual(second["read_latency"]["p50_us"], second["read_latency"]["p99_us"])
            with self.assertRaises(RuntimeError):
                support.read_workload(root, queries, ["bad"] * len(queries))
            for path in ["../escape", "/absolute", "C:\\escape", "file:stream"]:
                with self.assertRaises(ValueError):
                    support.read_workload(root, [{"path": path, "offset": 0, "length": 1}])

    def test_nonpassing_stage_cannot_be_promoted_by_exit_zero(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw); (root / "result.json").write_text('{"status":"FAIL"}')
            with self.assertRaises(RuntimeError): validation.stage_result(root)
            (root / "result.json").write_text('{"status":"PASS"}')
            self.assertEqual(validation.stage_result(root)["status"], "PASS")

    def test_generated_application_does_not_become_game_evidence(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw)
            (root / "result.json").write_text(json.dumps({
                "status": "PASS", "real_game_validation": "NOT RUN",
                "generated_fixture_detected": True,
            }))
            stage = validation.stage_result(root)
            self.assertEqual(stage["real_game_validation"], "NOT RUN")
            self.assertTrue(stage["generated_fixture_detected"])

    def test_cleanup_interrupt_is_recorded_without_aborting_cleanup(self):
        report = {"status": "PASS"}
        original = signal.getsignal(signal.SIGINT)
        with support.finalizing(report):
            signal.raise_signal(signal.SIGINT)
            report["cleanup_finished"] = True
        self.assertEqual(report["status"], "FAIL")
        self.assertEqual(report["interruption_during_cleanup"], "SIGINT")
        self.assertTrue(report["cleanup_finished"])
        self.assertEqual(signal.getsignal(signal.SIGINT), original)

    def test_dead_provider_still_attempts_owned_unmount(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw); mount = root / "mounted"; mount.mkdir()
            runner = mock.Mock(repo=root)
            provider = mock.Mock(returncode=0)
            provider.poll.return_value = 0
            api = mock.Mock()
            api.mount_record.return_value = {"real_mount": False}
            host = comparison.ReadOnlyMount(Path("cli.exe"), root / "base", mount,
                                            root, runner, {"commands": []}, "mount", api, "64M")
            host.process, host.entry, host.started = provider, {}, 0
            host.close(failed=True)
            self.assertEqual(runner.run.call_args.args[0], ["cli.exe", "unmount", str(mount)])

    def test_unknown_wof_flags_are_rejected(self):
        api = object.__new__(support.WindowsAPI)

        def native(path, external, provider, info, length):
            external._obj.value, provider._obj.value = 1, 2
            info[0], info[1] = 0, 1
            return 0

        api.wof = mock.Mock(WofIsExternalFile=native)
        with self.assertRaises(support.WindowsPrerequisite):
            api.wof_info("unused-path")

    def test_game_arguments_are_literal_argv_elements(self):
        args = validation.parse_args(["--work", "evidence path", "--game-path", "owned path",
                                      "--executable", "bin/game.exe", "--", "argument with spaces", "$(never-run); &"])
        command = validation.application_command(args, Path("cli path.exe"), Path("stage"), Path("receipt"))
        self.assertEqual(command[-2:], ["argument with spaces", "$(never-run); &"])

    @unittest.skipUnless(shutil.which("pwsh"), "PowerShell 7 argument handoff test")
    def test_powershell_handoff_preserves_unicode_and_literal_arguments(self):
        with tempfile.TemporaryDirectory() as raw:
            root = Path(raw); scripts = root / "tools"; scripts.mkdir()
            wrapper = scripts / "windows-hardware-validation.ps1"
            shutil.copyfile(TOOLS / wrapper.name, wrapper)
            captured = root / "captured.json"
            helper = scripts / "windows-validation.py"
            helper.write_text(
                "import json, pathlib, sys\n"
                "environment = json.load(sys.stdin)\n"
                f"pathlib.Path({str(captured)!r}).write_text(json.dumps({{'argv': sys.argv[1:], 'environment': environment}}), encoding='utf-8')\n",
                encoding="utf-8")

            def literal(value): return "'" + str(value).replace("'", "''") + "'"

            arguments = ["argument with spaces", "данные_日本", "$(never-run); &"]
            script = (f"& {literal(wrapper)} -EvidenceRoot {literal(root / 'evidence')} "
                      f"-Python {literal(sys.executable)} -GamePath {literal(root / 'owned game')} "
                      "-Executable 'bin/game.exe' -GameArguments @("
                      + ",".join(literal(value) for value in arguments) + ")")
            result = subprocess.run([shutil.which("pwsh"), "-NoLogo", "-NoProfile", "-Command", script],
                                    capture_output=True, timeout=60)
            self.assertEqual(result.returncode, 0, result.stderr.decode(errors="replace"))
            captured_json = json.loads(captured.read_text(encoding="utf-8"))
            self.assertEqual(captured_json["argv"][-4:], ["--", *arguments])
            self.assertIn("powershell", captured_json["environment"])


@unittest.skipUnless(sys.platform == "win32", "native Windows API/WOF evidence only")
class NativeWindowsTests(unittest.TestCase):
    def test_native_streams_resources_and_disposable_wof(self):
        api = support.WindowsAPI()
        with tempfile.TemporaryDirectory(prefix="playsparse-wof-api-") as raw:
            root = Path(raw); path = root / "compressible.bin"
            path.write_bytes(b"playsparse native WOF API\n" * 100000)
            if api.volume(root)["filesystem"] != "NTFS":
                self.skipTest("native disposable test requires NTFS")
            original = support.fingerprint(root, api)
            result = subprocess.run(comparison.compact_command(root, "XPRESS4K"), cwd=root,
                                    capture_output=True, timeout=60)
            self.assertEqual(result.returncode, 0, result.stderr.decode(errors="replace"))
            self.assertEqual(comparison.verified_wof(root, api, "XPRESS4K")["status"], "VERIFIED")
            compressed = support.fingerprint(root, api)
            self.assertEqual(support.content_identity(original), support.content_identity(compressed))
            self.assertGreater(compressed["allocated_data_bytes"], 0)
            resources = api.usage(os.getpid())
            self.assertGreater(resources["peak_rss_bytes"], 0)
            self.assertGreaterEqual(resources["cpu_seconds"], 0)


if __name__ == "__main__": unittest.main()
