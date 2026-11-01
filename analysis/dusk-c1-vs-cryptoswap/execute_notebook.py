#!/usr/bin/env python3
"""Execute this package's simple Python notebook without Jupyter dependencies."""

from __future__ import annotations

from contextlib import redirect_stdout
import io
import json
from pathlib import Path
import tempfile
import traceback


ROOT = Path(__file__).resolve().parent
NOTEBOOK = ROOT / "analysis.ipynb"


def main() -> None:
    notebook = json.loads(NOTEBOOK.read_text())
    namespace = {"__name__": "__notebook__"}
    execution_count = 0
    old_cwd = Path.cwd()
    try:
        import os

        os.chdir(ROOT)
        for cell in notebook["cells"]:
            if cell["cell_type"] != "code":
                continue
            execution_count += 1
            source = "".join(cell["source"])
            output = io.StringIO()
            try:
                with redirect_stdout(output):
                    exec(compile(source, f"analysis.ipynb:{execution_count}", "exec"), namespace)
            except Exception:
                cell["execution_count"] = execution_count
                cell["outputs"] = [
                    {
                        "output_type": "error",
                        "ename": "NotebookExecutionError",
                        "evalue": "code cell failed",
                        "traceback": traceback.format_exc().splitlines(),
                    }
                ]
                raise
            cell["execution_count"] = execution_count
            cell["outputs"] = [
                {
                    "output_type": "stream",
                    "name": "stdout",
                    "text": output.getvalue().splitlines(keepends=True),
                }
            ]
    finally:
        import os

        os.chdir(old_cwd)

    with tempfile.NamedTemporaryFile("w", dir=ROOT, delete=False) as temporary:
        json.dump(notebook, temporary, indent=1)
        temporary.write("\n")
        temporary_path = Path(temporary.name)
    temporary_path.replace(NOTEBOOK)
    print(f"executed {execution_count} code cells")


if __name__ == "__main__":
    main()
