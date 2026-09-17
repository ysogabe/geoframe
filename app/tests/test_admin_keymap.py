import subprocess
import unittest
from pathlib import Path


class AdminKeymapJavascriptRegressionTest(unittest.TestCase):
    def test_save_races_preserve_generation_and_late_edits(self):
        test_file = Path(__file__).with_name("js") / "admin-keymap-race.test.js"
        result = subprocess.run(
            ["node", str(test_file)],
            cwd=Path(__file__).resolve().parents[1],
            capture_output=True,
            text=True,
            check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("admin keymap race regressions: OK", result.stdout)


if __name__ == "__main__":
    unittest.main()
