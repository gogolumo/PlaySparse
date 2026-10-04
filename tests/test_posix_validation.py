"""Lifecycle fault injection; mocked mounts here are never runtime evidence."""
import contextlib
import importlib.util
import io
import hashlib
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

TOOLS = Path(__file__).resolve().parent.parent / "tools"
sys.path.insert(0, str(TOOLS))
import validation_common as common


def load(name, filename):
    spec = importlib.util.spec_from_file_location(name, TOOLS / filename)
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


owned = load("owned_application_test", "owned-application.py")
posix = load("posix_validation_test", "posix-runtime-validation.py")
sdk = load("macos_sdk_validation_test", "macos-sdk-check.py")


class LifecycleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="PlaySparse validation tests ")
        self.root = Path(self.temp.name)
        self.report = {"status": "FAIL", "commands": []}
        self.runner = common.CommandRunner(self.root, self.root, self.report, timeout=2, grace=1)

    def tearDown(self):
        self.temp.cleanup()

    def test_atomic_failure_preserves_previous_json(self):
        target = self.root / "result.json"
        common.atomic_json(target, {"old": True})
        with mock.patch.object(common.os, "replace", side_effect=OSError("injected ENOSPC")):
            with self.assertRaises(OSError):
                common.atomic_json(target, {"new": True})
        self.assertEqual(json.loads(target.read_text()), {"old": True})
        self.assertEqual(list(self.root.glob(".report-*")), [])

    def test_spawn_failure_records_fail_and_preserves_logs(self):
        with self.assertRaises(FileNotFoundError):
            self.runner.run([str(self.root / "missing")], "missing")
        result = json.loads((self.root / "result.json").read_text())
        self.assertEqual(result["commands"][0]["status"], "FAIL")
        self.assertIsNone(result["commands"][0]["exit_code"])
        self.assertTrue((self.root / "missing.stderr.log").is_file())

    def test_disconnected_linux_mount_is_found_without_stat(self):
        mountpoint = "/tmp/work with spaces/mounted"
        mountinfo = "42 1 0:55 / /tmp/work\\040with\\040spaces/mounted rw - fuse.playsparse PlaySparse rw\n"
        with mock.patch.object(common.sys, "platform", "linux"), \
             mock.patch.object(common.Path, "open", return_value=io.StringIO(mountinfo)), \
             mock.patch.object(common.os.path, "ismount", return_value=False):
            self.assertTrue(common.mount_present(mountpoint))

    def test_direct_argv_with_spaces_and_shell_metacharacters(self):
        text = "$HOME; touch should-not-exist"
        self.runner.run([sys.executable, "-c", "import json,sys; print(json.dumps(sys.argv[1:]))", text], "argv")
        self.assertEqual(json.loads((self.root / "argv.stdout.log").read_text()), [text])
        self.assertFalse((self.root / "should-not-exist").exists())

    def test_explicit_environment_reaches_child_without_secret_logging(self):
        env = dict(os.environ, PLAYSPARSE_VALIDATION_TEST="injected private fixture")
        self.runner.run([sys.executable, "-c", "import os; print(os.environ['PLAYSPARSE_VALIDATION_TEST'])"], "env", env=env)
        self.assertEqual((self.root / "env.stdout.log").read_text().strip(), env["PLAYSPARSE_VALIDATION_TEST"])
        self.assertNotIn("injected private fixture", (self.root / "result.json").read_text())

    def test_reused_and_overlapping_work_keeps_existing_evidence(self):
        repo, source = self.root / "repo", self.root / "source"
        repo.mkdir(); source.mkdir()
        work = common.safe_new_work(self.root / "work", repo, source)
        marker = work / "keep.json"
        marker.write_text("preserve")
        with self.assertRaises(common.PrerequisiteMissing):
            common.safe_new_work(work, repo, source)
        self.assertEqual(marker.read_text(), "preserve")
        for forbidden in (repo / "work", source / "work", self.root):
            with self.assertRaises(common.PrerequisiteMissing):
                common.safe_new_work(forbidden, repo, source)

    @unittest.skipIf(os.name == "nt", "symlink creation privilege varies on Windows")
    def test_dangling_work_symlink_is_refused(self):
        link = self.root / "work-link"
        destination = self.root / "absent"
        link.symlink_to(destination, target_is_directory=True)
        with self.assertRaises(common.PrerequisiteMissing):
            common.safe_new_work(link)
        self.assertFalse(destination.exists())

    @unittest.skipIf(os.name == "nt", "POSIX signals and process groups")
    def test_timeout_reaps_child(self):
        pid_file = self.root / "child.pid"
        code = f"import os,pathlib,time; pathlib.Path({str(pid_file)!r}).write_text(str(os.getpid())); time.sleep(60)"
        with self.assertRaises(subprocess.TimeoutExpired):
            self.runner.run([sys.executable, "-c", code], "timeout", timeout=.2)
        pid = int(pid_file.read_text())
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)
        result = json.loads((self.root / "result.json").read_text())
        self.assertEqual(result["commands"][0]["failure_type"], "TimeoutExpired")
        self.assertIsNotNone(result["commands"][0]["exit_code"])

    @unittest.skipIf(os.name == "nt", "POSIX signals and process groups")
    def test_sigint_and_sigterm_record_failure_and_reap_child(self):
        for signum in (signal.SIGINT, signal.SIGTERM):
            with self.subTest(signal=signum):
                work = self.root / str(signum)
                work.mkdir()
                pid_file = work / "child.pid"
                child_code = f"import os,pathlib,time; pathlib.Path({str(pid_file)!r}).write_text(str(os.getpid())); time.sleep(60)"
                code = f'''import pathlib,sys
sys.path.insert(0,{str(TOOLS)!r})
import validation_common as c
work=pathlib.Path({str(work)!r})
report={{"status":"FAIL","commands":[]}}
with c.signals():
 try:
  c.CommandRunner(work,work,report,grace=1).run([sys.executable,"-c",{child_code!r}],"child")
 except Exception as error:
  report["error"]=str(error)
 finally:
  c.atomic_json(work/"result.json",report)
'''
                with (work / "parent.log").open("w") as log:
                    process = subprocess.Popen([sys.executable, "-c", code], stdout=log, stderr=log)
                    deadline = time.monotonic() + 10
                    while not pid_file.exists() and time.monotonic() < deadline:
                        time.sleep(.02)
                    self.assertTrue(pid_file.exists())
                    process.send_signal(signum)
                    process.wait(timeout=10)
                with self.assertRaises(ProcessLookupError):
                    os.kill(int(pid_file.read_text()), 0)
                result = json.loads((work / "result.json").read_text())
                self.assertEqual(result["commands"][0]["status"], "FAIL")
                self.assertIn(signal.Signals(signum).name, result["error"])


class ProvenanceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="PlaySparse provenance ")
        self.root = Path(self.temp.name)
        self.repo = self.root / "repo"
        self.repo.mkdir()
        subprocess.run(["git", "init", "-q", str(self.repo)], check=True)
        for key, value in (("user.email", "validation@example.invalid"), ("user.name", "Validation Test")):
            subprocess.run(["git", "-C", str(self.repo), "config", key, value], check=True)
        (self.repo / "source.txt").write_text("original")
        subprocess.run(["git", "-C", str(self.repo), "add", "."], check=True)
        subprocess.run(["git", "-C", str(self.repo), "commit", "-q", "-m", "fixture"], check=True)
        self.binary = self.root / "executable with spaces"
        self.binary.write_text("fixture")
        self.binary.chmod(0o755)
        self.manifest = self.root / "provenance.json"

    def tearDown(self):
        self.temp.cleanup()

    def test_strict_dirty_and_unverified_gates(self):
        with self.assertRaisesRegex(common.PrerequisiteMissing, "receipt"):
            common.inspect_binary_provenance(self.repo, {"playsparse": self.binary})
        result = common.inspect_binary_provenance(self.repo, {"playsparse": self.binary}, allow_unverified=True)
        self.assertEqual(result["status"], "UNVERIFIED")
        (self.repo / "source.txt").write_text("changed")
        with self.assertRaisesRegex(common.PrerequisiteMissing, "Dirty"):
            common.inspect_binary_provenance(self.repo, {"playsparse": self.binary}, allow_unverified=True)
        self.assertTrue(common.inspect_binary_provenance(self.repo, {"playsparse": self.binary}, allow_dirty=True, allow_unverified=True)["repository"]["dirty"])

    def test_binary_and_source_mismatch_cannot_be_overridden(self):
        before = common.repository_identity(self.repo)
        common.write_build_manifest(self.manifest, self.repo, {"playsparse": self.binary}, before, ["fixture-build"])
        self.assertEqual(common.inspect_binary_provenance(self.repo, {"playsparse": self.binary}, self.manifest)["status"], "VERIFIED_LOCAL_BUILD_RECEIPT")
        self.binary.write_text("tampered")
        with self.assertRaisesRegex(common.PrerequisiteMissing, "hash mismatch"):
            common.inspect_binary_provenance(self.repo, {"playsparse": self.binary}, self.manifest, allow_unverified=True)
        (self.repo / "source.txt").write_text("changed")
        with self.assertRaisesRegex(common.PrerequisiteMissing, "source digest mismatch"):
            common.inspect_binary_provenance(self.repo, {"playsparse": self.binary}, self.manifest, allow_dirty=True, allow_unverified=True)

    def test_changed_source_during_build_has_no_receipt(self):
        before = common.repository_identity(self.repo)
        (self.repo / "source.txt").write_text("changed during build")
        with self.assertRaisesRegex(common.PrerequisiteMissing, "changed during"):
            common.write_build_manifest(self.manifest, self.repo, {"playsparse": self.binary}, before, ["build"])
        self.assertFalse(self.manifest.exists())


class IntegrationFaultTests(unittest.TestCase):
    def test_synthetic_game_stays_not_run_and_runtime_mutation_fails(self):
        for mutate_binary in (False, True):
            with self.subTest(mutate_binary=mutate_binary), tempfile.TemporaryDirectory(prefix="PlaySparse owned fixture ") as directory:
                root = Path(directory)
                source, work, binary = root / "source", root / "run", root / "playsparse"
                source.mkdir()
                (source / "application").write_text("owned fixture")
                (source / "fixture.json").write_text("{}")
                (source / "world.dat").write_bytes(b"synthetic asset")
                binary.write_text("original runtime")
                identity = {"git_sha": "fixture"}
                provenance = {"repository": identity,
                              "binaries": {"playsparse": {"sha256": common.digest(binary)}}}

                class FakeMount:
                    def __init__(self, *args):
                        self.record, self.events = {"test_only": True}, []
                    def __enter__(self):
                        return self
                    def __exit__(self, *args):
                        return False

                def command(runner, argv, label, **kwargs):
                    if label == "pack":
                        base = work / "base"
                        base.mkdir()
                        (base / "object").write_text("sealed base fixture")
                    if label == "application" and mutate_binary:
                        binary.write_text("changed runtime")
                    runner.report["commands"].append({"label": label, "exit_code": 0})
                    return {"exit_code": 0}

                with mock.patch.object(sys, "argv", ["owned", "--source", str(source), "--executable", "application", "--work", str(work), "--playsparse", str(binary), "--application-kind", "game"]), \
                     mock.patch.object(owned, "inspect_binary_provenance", return_value=provenance), \
                     mock.patch.object(owned, "repository_identity", return_value=identity), \
                     mock.patch.object(owned.updater, "Mount", FakeMount), \
                     mock.patch.object(common.CommandRunner, "run", command), \
                     contextlib.redirect_stdout(io.StringIO()):
                    self.assertEqual(owned.main(), 1 if mutate_binary else 0)
                result = json.loads((work / "evidence/result.json").read_text())
                self.assertEqual(result["real_game_validation"], "NOT RUN")
                self.assertTrue(result["generated_fixture_detected"])
                self.assertEqual(result["binary_unchanged_during_run"], not mutate_binary)
                self.assertTrue(result["source_unchanged"])
                self.assertTrue(result["base_unchanged"])

    def test_empty_directory_changes_installation_identity(self):
        with tempfile.TemporaryDirectory() as directory:
            source = Path(directory)
            (source / "file").write_bytes(b"unchanged")
            before = owned.fingerprint_tree(source)
            (source / "new-empty-directory").mkdir()
            after = owned.fingerprint_tree(source)
            self.assertNotEqual(before["tree_fingerprint_sha256"], after["tree_fingerprint_sha256"])

    def test_disk_shortage_blocks_before_commands(self):
        with tempfile.TemporaryDirectory(prefix="PlaySparse low disk ") as directory:
            work = Path(directory) / "run"
            env = {"available_disk_bytes": 0}
            with mock.patch.object(sys, "argv", ["validation", "--work", str(work), "--allow-dirty"]), \
                 mock.patch.object(posix, "environment", return_value=env), \
                 mock.patch.object(posix, "repository_identity", return_value={"dirty": False}), \
                 contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(posix.main(), 2)
            result = json.loads((work / "result.json").read_text())
            self.assertEqual(result["status"], "BLOCKED")
            self.assertEqual(result["commands"], [])
            self.assertEqual(result["stages"]["readonly"]["status"], "NOT RUN")

    def test_failed_application_still_verifies_source_and_base(self):
        with tempfile.TemporaryDirectory(prefix="PlaySparse failed application ") as directory:
            root = Path(directory)
            source, work, binary = root / "source", root / "run", root / "playsparse"
            source.mkdir()
            (source / "application").write_text("owned fixture")
            binary.write_text("binary fixture")

            class FakeMount:
                def __init__(self, cli, store, mountpoint, *args):
                    self.record, self.events = {"test_only": True}, []
                def __enter__(self):
                    return self
                def __exit__(self, *args):
                    return False

            def command(runner, argv, label, **kwargs):
                if label == "pack":
                    base = work / "base"
                    base.mkdir()
                    (base / "object").write_text("original base")
                if label == "application":
                    (work / "base/object").write_text("injected base mutation")
                    runner.report["commands"].append({"label": label, "exit_code": 17})
                    raise RuntimeError("injected failed application")
                return {"exit_code": 0}

            provenance = {"repository": {"git_sha": "fixture"}, "binaries": {"playsparse": {"sha256": "fixture"}}}
            with mock.patch.object(sys, "argv", ["owned", "--source", str(source), "--executable", "application", "--work", str(work), "--playsparse", str(binary)]), \
                 mock.patch.object(owned, "inspect_binary_provenance", return_value=provenance), \
                 mock.patch.object(owned, "repository_identity", return_value=provenance["repository"]), \
                 mock.patch.object(owned, "digest", return_value="fixture"), \
                 mock.patch.object(owned.updater, "Mount", FakeMount), \
                 mock.patch.object(common.CommandRunner, "run", command), \
                 contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(owned.main(), 1)
            result = json.loads((work / "evidence/result.json").read_text())
            self.assertEqual(result["child_exit_code"], 17)
            self.assertTrue(result["source_unchanged"])
            self.assertFalse(result["base_unchanged"])
            self.assertEqual(result["real_game_validation"], "NOT RUN")
            for name in ("source-before", "source-after", "base-before", "base-after"):
                self.assertTrue((work / f"evidence/{name}.json").is_file())


class SdkLifecycleTests(unittest.TestCase):
    """No real installer image or mount is used by these fault tests."""
    def run_sdk(self, root, mount_table, repository_after=None):
        work = root / "sdk-run"
        identity = {"git_sha": "fixture", "dirty": False, "working_tree_status": "",
                    "source_digest_sha256": "fixture"}
        installer = b"mock verified SDK image"

        def command(runner, argv, label, **kwargs):
            runner.report["commands"].append({"label": label, "command": argv, "exit_code": 0})
            if label == "signature":
                (work / "signature.stdout.log").write_text(
                    "Benjamin Fleischer (3T5GSNBU6W); trusted by the Apple notary service")
            return {"exit_code": 0}

        with mock.patch.object(sys, "argv", ["sdk", "--work", str(work)]), \
             mock.patch.object(sdk.sys, "platform", "darwin"), \
             mock.patch.object(sdk.platform, "platform", return_value="mock macOS"), \
             mock.patch.object(sdk, "SHA256", hashlib.sha256(installer).hexdigest()), \
             mock.patch.object(sdk.urllib.request, "urlopen", return_value=io.BytesIO(installer)), \
             mock.patch.object(sdk.common, "repository_identity", side_effect=[identity, repository_after or identity]), \
             mock.patch.object(sdk.common, "mount_present", side_effect=mount_table), \
             mock.patch.object(sdk.common.CommandRunner, "run", command), \
             contextlib.redirect_stdout(io.StringIO()):
            result = sdk.main()
        return result, json.loads((work / "result.json").read_text())

    def test_mount_table_failure_is_saved_and_signal_handlers_restore(self):
        with tempfile.TemporaryDirectory(prefix="PlaySparse SDK cleanup fault ") as directory:
            old_handlers = {number: signal.getsignal(number) for number in (signal.SIGINT, signal.SIGTERM)}
            with common.signals():
                code, result = self.run_sdk(Path(directory), RuntimeError("injected mount table failure"))
            self.assertEqual(code, 1)
            self.assertEqual(result["status"], "FAIL")
            self.assertEqual(result["image_cleanup"], "FAIL")
            self.assertIn("mount table failure", result["cleanup_error"])
            self.assertEqual(result["mount_validation"], "NOT RUN")
            for number, old in old_handlers.items():
                self.assertEqual(signal.getsignal(number), old)

    def test_retained_image_mount_cannot_pass(self):
        with tempfile.TemporaryDirectory(prefix="PlaySparse SDK retained image ") as directory:
            code, result = self.run_sdk(Path(directory), [True, True])
            self.assertEqual(code, 1)
            self.assertEqual(result["image_cleanup"], "FAIL")
            self.assertIn("remains attached", result["cleanup_error"])
            self.assertIn("detach", [entry["label"] for entry in result["commands"]])

    def test_repository_change_cannot_pass_even_when_commands_pass(self):
        with tempfile.TemporaryDirectory(prefix="PlaySparse SDK source mutation ") as directory:
            changed = {"git_sha": "fixture", "dirty": True, "working_tree_status": " M source.rs",
                       "source_digest_sha256": "changed"}
            code, result = self.run_sdk(Path(directory), [False, False], changed)
            self.assertEqual(code, 1)
            self.assertEqual(result["image_cleanup"], "PASS")
            self.assertFalse(result["repository_unchanged_during_run"])
            self.assertIn("changed during SDK", result["provenance_error"])


if __name__ == "__main__":
    unittest.main()
