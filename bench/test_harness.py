"""Fast harness self-checks; no benchmark timings or native build required.

Run with ``python -m unittest discover -s bench -p test_harness.py``.
"""

from __future__ import annotations

import copy
import datetime as dt
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import read_streaming
import worker
from fixtures import HEADER, iter_rows, make_rows


class ReadVerificationTests(unittest.TestCase):
    def setUp(self):
        self.rows = make_rows(7)
        worker._READ_RESULT = copy.deepcopy(self.rows)

    def tearDown(self):
        worker._READ_RESULT = None

    def verdict(self):
        return worker.verify_read("read_values", 7)[0]

    def test_generated_rows_and_numeric_representations(self):
        self.assertEqual(list(iter_rows(7)), self.rows)
        self.assertEqual(list(iter_rows(0)), [HEADER])
        worker._READ_RESULT[1][0] = 1.0
        self.assertTrue(self.verdict().startswith("ok"))

    def test_bool_integer_equality_does_not_hide_type_loss(self):
        worker._READ_RESULT[1][5] = int(worker._READ_RESULT[1][5])
        self.assertTrue(self.verdict().startswith("FAIL"))

    def test_date_serial_is_not_a_typed_date(self):
        worker._READ_RESULT[1][4] = 44973.0
        self.assertTrue(self.verdict().startswith("FAIL"))

    def test_midnight_date_and_datetime_are_equivalent(self):
        worker._READ_RESULT[1][4] = self.rows[1][4].date()
        self.assertTrue(self.verdict().startswith("ok"))
        worker._READ_RESULT[1][4] = self.rows[1][4] + dt.timedelta(hours=1)
        self.assertTrue(self.verdict().startswith("FAIL"))

    def test_shape_and_numeric_value_loss_are_detected(self):
        worker._READ_RESULT.pop()
        self.assertTrue(self.verdict().startswith("FAIL"))
        worker._READ_RESULT = copy.deepcopy(self.rows)
        worker._READ_RESULT[1][0] = True
        self.assertTrue(self.verdict().startswith("FAIL"))
        worker._READ_RESULT[1][0] = 2
        self.assertTrue(self.verdict().startswith("FAIL"))

    def test_observed_types_are_recorded(self):
        verdict, types = worker.verify_read("read_values", 7)
        self.assertTrue(verdict.startswith("ok"))
        self.assertEqual(types["5"], ["bool", "str"])
        self.assertEqual(types["0"], ["int", "str"])


class StreamingChecksumTests(unittest.TestCase):
    def test_generator_and_list_digests_match(self):
        self.assertEqual(read_streaming.consume(iter_rows(7)), read_streaming.consume(make_rows(7)))

    def test_consumer_limit_does_not_request_an_extra_row(self):
        def rows():
            yield [1]
            yield [2]
            raise AssertionError("consumer read beyond requested rows")

        self.assertEqual(read_streaming.consume(rows(), 2)["count"], 2)

    def test_checksum_distinguishes_bool_and_number(self):
        numeric = read_streaming.consume([[1]])
        boolean = read_streaming.consume([[True]])
        self.assertNotEqual(numeric["checksum"], boolean["checksum"])
        self.assertEqual(numeric["checksum"], read_streaming.consume([[1.0]])["checksum"])

    def test_checksum_distinguishes_date_and_serial(self):
        date = read_streaming.consume([[dt.date(2024, 1, 1)]])
        datetime = read_streaming.consume([[dt.datetime(2024, 1, 1)]])
        self.assertEqual(date["checksum"], datetime["checksum"])
        self.assertNotEqual(date["checksum"], read_streaming.consume([[45292.0]])["checksum"])


class RegressionGateTests(unittest.TestCase):
    def test_new_task_correctness_failure_is_not_ignored(self):
        with tempfile.TemporaryDirectory() as directory:
            base = Path(directory) / "base.json"
            after = Path(directory) / "after.json"
            base.write_text(json.dumps({"rows": []}))
            after.write_text(
                json.dumps(
                    {
                        "rows": [
                            {"task": "new", "size": "small", "lib": "kyrax", "verify": "FAIL type"}
                        ]
                    }
                )
            )
            completed = subprocess.run(
                [
                    sys.executable,
                    str(Path(__file__).with_name("compare.py")),
                    str(base),
                    str(after),
                ],
                capture_output=True,
                text=True,
            )
            self.assertEqual(completed.returncode, 1, completed.stdout + completed.stderr)
            self.assertIn("verify FAILED", completed.stdout)


if __name__ == "__main__":
    unittest.main()
