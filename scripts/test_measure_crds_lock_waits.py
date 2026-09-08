"""Offline interval-analysis tests; no BPF attachment or validator required."""

import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

import crds_lock_profile


spec = importlib.util.spec_from_file_location(
    "measure_crds_lock_waits", Path(__file__).with_name("measure-crds-lock-waits.py")
)
measure = importlib.util.module_from_spec(spec)
spec.loader.exec_module(measure)


class ReaderQueueDependenciesTest(unittest.TestCase):
    def test_reader_writer_reader_chain(self):
        # Reader 1 owns the lock until 100; writer queues at 10 and acquires at
        # 100; reader 2 arrives at 20 and acquires after the writer releases.
        waits = [("write", 2, 102, 10, 100), ("read", 3, 103, 20, 102)]
        holds = [("read", 1, 101, 0, 100), ("write", 2, 102, 100, 101)]
        blockers, last = measure.find_blockers(waits, holds)
        self.assertEqual(last[1]["mode"], "write")
        self.assertEqual(blockers[1][0]["overlap_ns"], 1)
        dependencies = measure.find_reader_queue_dependencies(waits, blockers)
        self.assertEqual(len(dependencies), 1)
        self.assertEqual(dependencies[0]["wait_id"], 1)
        self.assertEqual(dependencies[0]["writer_wait_id"], 0)
        self.assertEqual(dependencies[0]["holder_ip"], 1)
        self.assertEqual(dependencies[0]["overlap_start_ns"], 20)
        self.assertEqual(dependencies[0]["overlap_end_ns"], 100)
        symbolic = measure.symbolize_reader_dependencies(
            dependencies, waits, {1: "reader_one", 2: "writer", 3: "reader_two"}
        )[0]
        self.assertEqual(symbolic["holder"], "reader_one")
        self.assertEqual(symbolic["waiter"], "reader_two")
        self.assertEqual(symbolic["via_writer"], "writer")

    def test_read_overlap_alone_is_not_a_dependency(self):
        waits = [("read", 3, 103, 20, 102)]
        holds = [("read", 1, 101, 0, 100)]
        blockers, last = measure.find_blockers(waits, holds)
        self.assertEqual(blockers, [[]])
        self.assertEqual(last, [None])
        self.assertEqual(measure.find_reader_queue_dependencies(waits, blockers), [])

    def test_writer_arriving_later_is_not_assumed_to_precede_reader(self):
        waits = [("write", 2, 102, 30, 100), ("read", 3, 103, 20, 102)]
        holds = [("read", 1, 101, 0, 100)]
        blockers, _ = measure.find_blockers(waits, holds)
        self.assertEqual(measure.find_reader_queue_dependencies(waits, blockers), [])

    def test_completed_writer_is_not_a_queue_dependency(self):
        waits = [("write", 2, 102, 10, 19), ("read", 3, 103, 20, 102)]
        holds = [("read", 1, 101, 0, 18), ("write", 2, 102, 19, 100)]
        blockers, _ = measure.find_blockers(waits, holds)
        self.assertEqual(measure.find_reader_queue_dependencies(waits, blockers), [])

    def test_empty_capture(self):
        self.assertEqual(measure.find_blockers([], []), ([], []))
        self.assertEqual(measure.find_reader_queue_dependencies([], []), [])


class CallerLookupTest(unittest.TestCase):
    def test_wrapper_ranges_use_elf_load_bias(self):
        wrapper = "<solana_gossip::crds_rwlock::CrdsRwLock>::write"
        symbols = f"00002000 00000100 T {wrapper}\n00003000 00000100 T real_caller\n"
        segments = "LOAD 0x000000 0x000000 0x000000\nLOAD 0x001000 0x002000 0x002000\n"
        mappings = "100000-101000 r--p 00000000 00:00 1 /validator\n102000-103000 r-xp 00001000 00:00 1 /validator\n"
        with patch.object(crds_lock_profile.subprocess, "check_output", side_effect=[segments, symbols]), patch.object(Path, "read_text", return_value=mappings):
            code, names = crds_lock_profile.caller_lookup_bpf("/validator", 1)
        self.assertEqual(names, [wrapper])
        self.assertIn(f"ip >= {0x102000}ULL && ip < {0x102100}ULL", code)
        self.assertNotIn("real_caller", code)

    def test_no_wrapper_symbols(self):
        segments = "LOAD 0x000000 0x400000 0x400000\n"
        mappings = "400000-401000 r--p 00000000 00:00 1 /validator\n"
        with patch.object(crds_lock_profile.subprocess, "check_output", side_effect=[segments, ""]), patch.object(Path, "read_text", return_value=mappings):
            code, names = crds_lock_profile.caller_lookup_bpf("/validator", 1)
        self.assertEqual(names, [])
        self.assertIn("return 0;", code)


if __name__ == "__main__":
    unittest.main()
