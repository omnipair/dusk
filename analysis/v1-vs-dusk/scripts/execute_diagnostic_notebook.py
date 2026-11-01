#!/usr/bin/env python3
"""Execute the dependency-free diagnostic notebook in place."""

from __future__ import annotations

import argparse
import contextlib
import io
import json
import os
import traceback
from pathlib import Path


def stream_output(name: str, value: str) -> dict:
    return {"name": name, "output_type": "stream", "text": value.splitlines(keepends=True)}


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("notebook", type=Path)
    parser.add_argument("--working-directory", type=Path)
    args = parser.parse_args()

    notebook_path = args.notebook.resolve()
    working_directory = (args.working_directory or notebook_path.parent).resolve()
    notebook = json.loads(notebook_path.read_text(encoding="utf-8"))
    if notebook.get("nbformat") != 4 or not isinstance(notebook.get("cells"), list):
        raise RuntimeError("unsupported notebook document")

    namespace = {"__name__": "__main__"}
    execution_count = 0
    prior_directory = Path.cwd()
    os.chdir(working_directory)
    try:
        for index, cell in enumerate(notebook["cells"]):
            if cell.get("cell_type") != "code":
                continue
            execution_count += 1
            cell["execution_count"] = execution_count
            cell["outputs"] = []
            source = "".join(cell.get("source", []))
            stdout = io.StringIO()
            stderr = io.StringIO()
            try:
                with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                    exec(compile(source, f"{notebook_path.name}:cell-{index}", "exec"), namespace)
            except Exception as error:
                if stdout.getvalue():
                    cell["outputs"].append(stream_output("stdout", stdout.getvalue()))
                if stderr.getvalue():
                    cell["outputs"].append(stream_output("stderr", stderr.getvalue()))
                cell["outputs"].append(
                    {
                        "ename": type(error).__name__,
                        "evalue": str(error),
                        "output_type": "error",
                        "traceback": traceback.format_exc().splitlines(),
                    }
                )
                raise
            if stdout.getvalue():
                cell["outputs"].append(stream_output("stdout", stdout.getvalue()))
            if stderr.getvalue():
                cell["outputs"].append(stream_output("stderr", stderr.getvalue()))
    finally:
        os.chdir(prior_directory)
        partial = notebook_path.with_suffix(notebook_path.suffix + ".partial")
        partial.write_text(json.dumps(notebook, indent=2) + "\n", encoding="utf-8")
        partial.replace(notebook_path)

    errors = [
        output
        for cell in notebook["cells"]
        if cell.get("cell_type") == "code"
        for output in cell.get("outputs", [])
        if output.get("output_type") == "error"
    ]
    if errors:
        raise RuntimeError(f"notebook completed with {len(errors)} error output(s)")
    print(f"executed {execution_count} code cells: {notebook_path}")


if __name__ == "__main__":
    main()
