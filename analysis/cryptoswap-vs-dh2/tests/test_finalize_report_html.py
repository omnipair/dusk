import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
if str(ROOT) not in sys.path:
    sys.path.insert(0, str(ROOT))

from finalize_report_html import STYLE_ID, finalize, inject_print_style


class FinalizeReportHtmlTests(unittest.TestCase):
    def test_injects_before_head_close_without_changing_screen_css(self) -> None:
        source = "<html><head><style>@media screen{table{width:max-content}}</style></head><body></body></html>"
        result = inject_print_style(source)
        self.assertIn(f'id="{STYLE_ID}"', result)
        self.assertIn("@media screen{table{width:max-content}}", result)
        self.assertLess(result.index(f'id="{STYLE_ID}"'), result.index("</head>"))

    def test_finalize_is_atomic_and_idempotent(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "report.html"
            path.write_text("<html><head></head><body></body></html>", encoding="utf-8")
            self.assertTrue(finalize(path))
            once = path.read_text(encoding="utf-8")
            self.assertFalse(finalize(path))
            self.assertEqual(once, path.read_text(encoding="utf-8"))
            self.assertEqual(once.count(f'id="{STYLE_ID}"'), 1)

    def test_existing_style_is_replaced_when_rules_change(self) -> None:
        source = (
            f'<html><head><style id="{STYLE_ID}">old rules</style></head>'
            "<body></body></html>"
        )
        result = inject_print_style(source)
        self.assertNotIn("old rules", result)
        self.assertIn("table-layout: fixed", result)
        self.assertEqual(result.count(f'id="{STYLE_ID}"'), 1)

    def test_missing_head_close_is_rejected(self) -> None:
        with self.assertRaisesRegex(ValueError, "missing </head>"):
            inject_print_style("<html><head><body></body></html>")


if __name__ == "__main__":
    unittest.main()
