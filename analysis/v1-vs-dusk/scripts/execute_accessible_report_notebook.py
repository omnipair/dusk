#!/usr/bin/env python3
"""Execute the standard-library-only companion report notebook in place."""

from __future__ import annotations

from contextlib import redirect_stdout
import io
import json
import os
from pathlib import Path
import tempfile


ROOT = Path(__file__).resolve().parents[1]
NOTEBOOK = ROOT / "results" / "scoped-three-market" / "report" / "scoped-benchmark.ipynb"


def main() -> None:
    notebook = json.loads(NOTEBOOK.read_text(encoding="utf-8"))
    namespace: dict[str, object] = {}
    execution_count = 0
    previous_cwd = Path.cwd()
    os.chdir(NOTEBOOK.parent)
    try:
        for cell in notebook["cells"]:
            if cell.get("cell_type") != "code":
                continue
            execution_count += 1
            stream = io.StringIO()
            try:
                with redirect_stdout(stream):
                    exec("".join(cell.get("source", [])), namespace, namespace)
            except Exception as error:
                cell["execution_count"] = execution_count
                cell["outputs"] = [
                    {
                        "output_type": "error",
                        "ename": type(error).__name__,
                        "evalue": str(error),
                        "traceback": [],
                    }
                ]
                raise
            cell["execution_count"] = execution_count
            cell["outputs"] = [
                {
                    "output_type": "stream",
                    "name": "stdout",
                    "text": stream.getvalue().splitlines(keepends=True),
                }
            ]
    finally:
        os.chdir(previous_cwd)
    with tempfile.NamedTemporaryFile(
        "w", encoding="utf-8", dir=NOTEBOOK.parent, delete=False
    ) as handle:
        json.dump(notebook, handle, indent=2)
        handle.write("\n")
        temporary = Path(handle.name)
    temporary.replace(NOTEBOOK)
    print(f"executed {execution_count} code cell(s): {NOTEBOOK}")


if __name__ == "__main__":
    main()
