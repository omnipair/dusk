#!/usr/bin/env python3
"""Execute this study's simple Python notebook without third-party packages."""

from __future__ import annotations

import argparse
import contextlib
import io
import json
import os
import traceback
from pathlib import Path


def execute(path: Path) -> None:
    notebook = json.loads(path.read_text(encoding="utf-8"))
    namespace: dict[str, object] = {"__name__": "__notebook__"}
    previous_cwd = Path.cwd()
    os.chdir(path.parent)
    try:
        execution_count = 0
        for index, cell in enumerate(notebook.get("cells", [])):
            if cell.get("cell_type") != "code":
                continue
            execution_count += 1
            cell["execution_count"] = execution_count
            cell["outputs"] = []
            stdout = io.StringIO()
            stderr = io.StringIO()
            try:
                with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                    exec(compile(cell.get("source", ""), f"{path.name}:cell-{index + 1}", "exec"), namespace)
            except Exception as exc:
                if stdout.getvalue():
                    cell["outputs"].append({"name": "stdout", "output_type": "stream", "text": stdout.getvalue()})
                if stderr.getvalue():
                    cell["outputs"].append({"name": "stderr", "output_type": "stream", "text": stderr.getvalue()})
                cell["outputs"].append(
                    {
                        "ename": type(exc).__name__,
                        "evalue": str(exc),
                        "output_type": "error",
                        "traceback": traceback.format_exc().splitlines(),
                    }
                )
                notebook.setdefault("metadata", {})["execution_status"] = "failed"
                path.write_text(json.dumps(notebook, indent=2) + "\n", encoding="utf-8")
                raise
            if stdout.getvalue():
                cell["outputs"].append({"name": "stdout", "output_type": "stream", "text": stdout.getvalue()})
            if stderr.getvalue():
                cell["outputs"].append({"name": "stderr", "output_type": "stream", "text": stderr.getvalue()})
        notebook.setdefault("metadata", {})["execution_status"] = "completed"
        path.write_text(json.dumps(notebook, indent=2) + "\n", encoding="utf-8")
    finally:
        os.chdir(previous_cwd)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("notebook", type=Path)
    args = parser.parse_args()
    execute(args.notebook.resolve())
    print(args.notebook.resolve())


if __name__ == "__main__":
    main()
