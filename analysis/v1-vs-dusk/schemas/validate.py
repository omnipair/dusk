#!/usr/bin/env python3
"""Validate the committed, hand-maintained benchmark schema snapshot set.

This deliberately does not validate runtime artifacts. Rust validators remain
authoritative for self-hashes, cross-record invariants, and chain semantics.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any, Iterable


SCHEMA_DIR = Path(__file__).resolve().parent
MODULE_DIR = SCHEMA_DIR.parent
CATALOG_PATH = SCHEMA_DIR / "catalog.v1.json"
JSON_SCHEMA_DRAFT = "https://json-schema.org/draft/2020-12/schema"
ALLOWED_SCOPES = {
    "catalog_contract",
    "supporting_definitions",
    "full_structural",
    "envelope_and_variant_tag",
    "top_level_and_metric_structures",
    "portable_builder_input_subset",
}


class ValidationError(RuntimeError):
    """A snapshot-set integrity or metadata validation failure."""


def _reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise ValidationError(f"duplicate JSON object key: {key!r}")
        value[key] = item
    return value


def load_json_strict(path: Path) -> Any:
    try:
        return json.loads(
            path.read_text(encoding="utf-8"),
            object_pairs_hook=_reject_duplicate_keys,
            parse_constant=lambda value: (_ for _ in ()).throw(
                ValidationError(f"non-finite JSON number in {path.name}: {value}")
            ),
        )
    except (OSError, UnicodeError, json.JSONDecodeError) as error:
        raise ValidationError(f"cannot read strict JSON {path}: {error}") from error


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def _require(condition: bool, message: str) -> None:
    if not condition:
        raise ValidationError(message)


def _walk(value: Any, location: str = "$") -> Iterable[tuple[str, Any]]:
    yield location, value
    if isinstance(value, dict):
        for key, child in value.items():
            yield from _walk(child, f"{location}.{key}")
    elif isinstance(value, list):
        for index, child in enumerate(value):
            yield from _walk(child, f"{location}[{index}]")


def _safe_schema_file(name: Any) -> str:
    _require(isinstance(name, str) and name.endswith(".schema.json"),
             f"invalid schema_file: {name!r}")
    _require(Path(name).name == name, f"schema_file must be a basename: {name!r}")
    return name


def _resolve_json_pointer(document: Any, fragment: str, origin: str) -> Any:
    _require(fragment.startswith("/"), f"unsupported JSON pointer in {origin}: #{fragment}")
    current = document
    for encoded in fragment[1:].split("/"):
        token = encoded.replace("~1", "/").replace("~0", "~")
        if isinstance(current, dict) and token in current:
            current = current[token]
        elif isinstance(current, list) and token.isdigit() and int(token) < len(current):
            current = current[int(token)]
        else:
            raise ValidationError(f"unresolved JSON pointer in {origin}: #{fragment}")
    return current


def _safe_rust_source(value: Any, origin: str) -> Path:
    _require(isinstance(value, str) and Path(value).as_posix() == value,
             f"{origin}: invalid rust_source")
    relative = Path(value)
    _require(not relative.is_absolute() and ".." not in relative.parts,
             f"{origin}: rust_source escapes module root")
    path = MODULE_DIR / relative
    _require(path.is_file(), f"{origin}: missing Rust source {value}")
    return path


def _rust_public_struct_fields(source: str, symbol: str, origin: str) -> set[str]:
    start = source.find(symbol)
    _require(start >= 0, f"{origin}: Rust symbol not found: {symbol!r}")
    opening = source.find("{", start + len(symbol))
    _require(opening >= 0, f"{origin}: Rust struct has no body: {symbol!r}")
    depth = 0
    closing = None
    for index in range(opening, len(source)):
        character = source[index]
        if character == "{":
            depth += 1
        elif character == "}":
            depth -= 1
            if depth == 0:
                closing = index
                break
    _require(closing is not None, f"{origin}: unterminated Rust struct: {symbol!r}")
    body = source[opening + 1:closing]
    fields = set(re.findall(r"(?m)^\s*pub\s+([A-Za-z_][A-Za-z0-9_]*)\s*:", body))
    _require(fields, f"{origin}: no public fields found for {symbol!r}")
    return fields


def _validate_rust_field_contract(
    schema: Any,
    schema_pointer: str,
    rust_source: Path,
    rust_symbol: str,
    origin: str,
) -> None:
    _require(schema_pointer == "#" or schema_pointer.startswith("#/"),
             f"{origin}: schema_pointer must be a local JSON pointer")
    target = schema if schema_pointer == "#" else _resolve_json_pointer(
        schema, schema_pointer[1:], origin
    )
    _require(isinstance(target, dict) and target.get("type") == "object",
             f"{origin}: field-bound schema target must be an object")
    _require(target.get("additionalProperties") is False,
             f"{origin}: field-bound schema target must reject unknown fields")
    properties = target.get("properties")
    required = target.get("required")
    _require(isinstance(properties, dict) and isinstance(required, list),
             f"{origin}: field-bound schema target needs properties and required")
    rust_fields = _rust_public_struct_fields(
        rust_source.read_text(encoding="utf-8"), rust_symbol, origin
    )
    schema_fields = set(properties)
    required_fields = set(required)
    _require(schema_fields == rust_fields,
             f"{origin}: schema/Rust field mismatch: "
             f"schema_only={sorted(schema_fields - rust_fields)}, "
             f"rust_only={sorted(rust_fields - schema_fields)}")
    _require(required_fields == rust_fields,
             f"{origin}: exact versioned contract must require every Rust field: "
             f"optional={sorted(rust_fields - required_fields)}, "
             f"unknown={sorted(required_fields - rust_fields)}")


def _validate_schema_shape(name: str, schema: Any) -> None:
    _require(isinstance(schema, dict), f"{name}: schema root must be an object")
    _require(schema.get("$schema") == JSON_SCHEMA_DRAFT,
             f"{name}: expected JSON Schema draft 2020-12")
    _require(isinstance(schema.get("$id"), str) and schema["$id"],
             f"{name}: missing $id")
    _require(schema.get("x-snapshot-status") == "hand_maintained",
             f"{name}: snapshot status must disclose hand_maintained")
    _require(schema.get("x-validation-scope") in ALLOWED_SCOPES,
             f"{name}: invalid x-validation-scope")
    _require(isinstance(schema.get("x-authoritative-rust-type"), str)
             and schema["x-authoritative-rust-type"],
             f"{name}: missing x-authoritative-rust-type")

    for location, value in _walk(schema):
        if isinstance(value, dict):
            required = value.get("required")
            if required is not None:
                _require(isinstance(required, list)
                         and all(isinstance(key, str) for key in required),
                         f"{name} {location}: required must be a string array")
                _require(len(required) == len(set(required)),
                         f"{name} {location}: duplicate required member")
                properties = value.get("properties")
                if isinstance(properties, dict):
                    missing = sorted(set(required) - set(properties))
                    _require(not missing,
                             f"{name} {location}: required keys absent from properties: {missing}")


def _validate_refs(name: str, schema: Any, documents: dict[str, Any]) -> None:
    for location, value in _walk(schema):
        if not isinstance(value, dict) or "$ref" not in value:
            continue
        reference = value["$ref"]
        _require(isinstance(reference, str) and reference,
                 f"{name} {location}: $ref must be a nonempty string")
        target_name, separator, fragment = reference.partition("#")
        if target_name:
            target_name = _safe_schema_file(target_name)
            _require(target_name in documents,
                     f"{name} {location}: uncommitted schema reference {target_name}")
            target = documents[target_name]
        else:
            target = schema
        if separator and fragment:
            _resolve_json_pointer(target, fragment, f"{name} {location}")


def _rust_u16_constant(source: Path, constant: str, origin: str) -> int:
    match = re.search(
        rf"(?m)^pub\s+const\s+{re.escape(constant)}\s*:\s*u16\s*=\s*([0-9]+)\s*;",
        source.read_text(encoding="utf-8"),
    )
    _require(match is not None, f"{origin}: Rust schema-version constant not found")
    return int(match.group(1))


def _validate_leverage_contracts(
    documents: dict[str, Any], artifacts: list[dict[str, Any]]
) -> None:
    """Prevent the campaign schema and embedded cell schema from drifting apart."""
    benchmark_name = "leverage-benchmark-result-v2.schema.json"
    campaign_name = "leverage-campaign-v2.schema.json"
    _require(benchmark_name in documents, "current leverage result schema v2 is missing")
    _require(campaign_name in documents, "leverage campaign schema v1 is missing")
    benchmark = documents[benchmark_name]
    campaign = documents[campaign_name]

    by_logical_name = {record.get("logical_name"): record for record in artifacts}
    benchmark_record = by_logical_name.get("leverage_benchmark_result")
    campaign_record = by_logical_name.get("leverage_campaign")
    _require(isinstance(benchmark_record, dict), "leverage result catalog entry is missing")
    _require(isinstance(campaign_record, dict), "leverage campaign catalog entry is missing")
    _require(benchmark_record.get("record_schema_version") == 2,
             "leverage result catalog version must be 2")
    _require(campaign_record.get("record_schema_version") == 2,
             "leverage campaign catalog version must be 2")
    _require(benchmark.get("properties", {}).get("schema_version", {}).get("const") == 2,
             "leverage result schema_version must be const 2")
    _require(campaign.get("properties", {}).get("schema_version", {}).get("const") == 2,
             "leverage campaign schema_version must be const 2")

    benchmark_rust_version = _rust_u16_constant(
        MODULE_DIR / "src/leverage.rs", "LEVERAGE_BENCHMARK_SCHEMA_VERSION", benchmark_name
    )
    campaign_rust_version = _rust_u16_constant(
        MODULE_DIR / "src/leverage_campaign.rs", "LEVERAGE_CAMPAIGN_SCHEMA_VERSION", campaign_name
    )
    _require(benchmark_rust_version == 2,
             "leverage result schema does not match the Rust version constant")
    _require(campaign_rust_version == 2,
             "leverage campaign schema does not match the Rust version constant")

    campaign_refs = {
        value["$ref"]
        for _, value in _walk(campaign)
        if isinstance(value, dict) and isinstance(value.get("$ref"), str)
    }
    _require(benchmark_name in campaign_refs,
             "leverage campaign cells must reference the current result schema v2")

    for name, document in ((benchmark_name, benchmark), (campaign_name, campaign)):
        for location, value in _walk(document):
            if isinstance(value, dict) and value.get("type") == "object":
                _require(value.get("additionalProperties") is False,
                         f"{name} {location}: versioned object must reject unknown fields")


def validate_snapshot_set(catalog_path: Path = CATALOG_PATH) -> tuple[int, int]:
    catalog = load_json_strict(catalog_path)
    _require(isinstance(catalog, dict), "catalog root must be an object")
    _require(catalog.get("catalog_schema_version") == 1,
             "catalog_schema_version must be 1")
    _require(catalog.get("status") == "committed_hand_maintained_snapshots",
             "catalog must disclose hand-maintained snapshot status")
    _require(catalog.get("runtime_generation") is False,
             "runtime_generation must be false")
    _require(catalog.get("catalog_validator") == Path(__file__).name,
             "catalog_validator must identify this validator")

    supporting = catalog.get("supporting_schemas")
    artifacts = catalog.get("artifacts")
    _require(isinstance(supporting, list) and supporting,
             "supporting_schemas must be a nonempty array")
    _require(isinstance(artifacts, list) and artifacts,
             "artifacts must be a nonempty array")
    records = supporting + artifacts

    schema_names: set[str] = set()
    schema_ids: set[str] = set()
    logical_names: set[str] = set()
    documents: dict[str, Any] = {}

    for record in records:
        _require(isinstance(record, dict), "catalog entries must be objects")
        name = _safe_schema_file(record.get("schema_file"))
        _require(name not in schema_names, f"duplicate catalog schema_file: {name}")
        schema_names.add(name)

        schema_path = SCHEMA_DIR / name
        _require(schema_path.is_file(), f"missing committed schema: {name}")
        schema = load_json_strict(schema_path)
        _validate_schema_shape(name, schema)
        documents[name] = schema

        schema_id = record.get("schema_id")
        _require(schema_id == schema.get("$id"),
                 f"{name}: catalog schema_id does not match $id")
        _require(schema_id not in schema_ids, f"duplicate catalog schema_id: {schema_id}")
        schema_ids.add(schema_id)

        scope = record.get("validation_scope")
        _require(scope == schema.get("x-validation-scope"),
                 f"{name}: catalog validation_scope does not match schema")
        _require(record.get("schema_sha256") == sha256_file(schema_path),
                 f"{name}: schema_sha256 mismatch")

    for record in artifacts:
        logical_name = record.get("logical_name")
        _require(isinstance(logical_name, str) and logical_name,
                 "artifact logical_name must be nonempty")
        _require(logical_name not in logical_names,
                 f"duplicate artifact logical_name: {logical_name}")
        logical_names.add(logical_name)

        name = record["schema_file"]
        _require(record.get("rust_type") == documents[name].get("x-authoritative-rust-type"),
                 f"{name}: catalog rust_type does not match schema")
        rust_source = record.get("rust_source")
        source_path = _safe_rust_source(rust_source, name)
        symbol = record.get("rust_symbol")
        _require(isinstance(symbol, str) and symbol in source_path.read_text(encoding="utf-8"),
                 f"{name}: Rust symbol not found in {rust_source}: {symbol!r}")
        if record.get("root_fields_match_rust") is True:
            _validate_rust_field_contract(
                documents[name], "#", source_path, symbol, f"{name} root"
            )
        nested = record.get("nested_rust_contracts", [])
        _require(isinstance(nested, list),
                 f"{name}: nested_rust_contracts must be an array")
        for binding in nested:
            _require(isinstance(binding, dict),
                     f"{name}: nested Rust binding must be an object")
            nested_source = _safe_rust_source(binding.get("rust_source"), name)
            nested_symbol = binding.get("rust_symbol")
            pointer = binding.get("schema_pointer")
            _require(isinstance(nested_symbol, str) and isinstance(pointer, str),
                     f"{name}: malformed nested Rust binding")
            _validate_rust_field_contract(
                documents[name], pointer, nested_source, nested_symbol,
                f"{name} {pointer}",
            )

    for name, schema in documents.items():
        _validate_refs(name, schema, documents)

    _validate_leverage_contracts(documents, artifacts)

    committed = {path.name for path in SCHEMA_DIR.glob("*.schema.json")}
    _require(committed == schema_names,
             "catalog/schema file mismatch: "
             f"uncataloged={sorted(committed - schema_names)}, "
             f"missing={sorted(schema_names - committed)}")
    return len(artifacts), len(supporting)


def self_test() -> None:
    parsed = json.loads('{"a": 1}', object_pairs_hook=_reject_duplicate_keys)
    _require(parsed == {"a": 1}, "strict JSON parser self-test failed")
    try:
        json.loads('{"a": 1, "a": 2}', object_pairs_hook=_reject_duplicate_keys)
    except ValidationError:
        pass
    else:
        raise ValidationError("strict JSON parser accepted a duplicate key")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-test", action="store_true",
                        help="also exercise duplicate-key rejection")
    args = parser.parse_args()
    try:
        artifact_count, supporting_count = validate_snapshot_set()
        if args.self_test:
            self_test()
    except ValidationError as error:
        print(f"schema validation failed: {error}", file=sys.stderr)
        return 1
    suffix = " (self-test passed)" if args.self_test else ""
    print(
        f"validated {artifact_count} artifact snapshots + "
        f"{supporting_count} supporting schemas{suffix}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
