#!/usr/bin/env python3
"""Execute the trusted local scoped-benchmark notebook with the stdlib only."""

from __future__ import annotations

import argparse
from contextlib import redirect_stderr, redirect_stdout
import io
import json
import os
from pathlib import Path


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("notebook", type=Path)
    args = parser.parse_args()
    notebook_path = args.notebook.resolve()
    notebook = json.loads(notebook_path.read_text(encoding="utf-8"))
    namespace: dict[str, object] = {"__name__": "__scoped_benchmark_notebook__"}
    execution_count = 0
    original_directory = Path.cwd()
    try:
        os.chdir(notebook_path.parent)
        for cell in notebook["cells"]:
            if cell.get("cell_type") != "code":
                continue
            execution_count += 1
            output = io.StringIO()
            source = "".join(cell.get("source", []))
            with redirect_stdout(output), redirect_stderr(output):
                exec(compile(source, f"{notebook_path.name}:cell-{execution_count}", "exec"), namespace)
            cell["execution_count"] = execution_count
            text = output.getvalue()
            cell["outputs"] = (
                [{"name": "stdout", "output_type": "stream", "text": text.splitlines(keepends=True)}]
                if text
                else []
            )
    finally:
        os.chdir(original_directory)
    notebook_path.write_text(json.dumps(notebook, indent=2) + "\n", encoding="utf-8")
    print(f"executed {execution_count} code cells: {notebook_path}")


if __name__ == "__main__":
    main()
