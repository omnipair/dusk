-- Generated implementation-status evidence for the Dusk versus V1 benchmark.
-- This is implementation evidence, not historical performance data.
WITH source_inventory(artifact, purpose, sha256) AS (
  VALUES
    ('analysis/v1-vs-dusk/README.md', 'Operator entry point and current status', '4c439ef3f1def12be5a7fdc0d4fc2762903c7faf3a135744f1e4ffe4cc695a60'),
    ('analysis/v1-vs-dusk/docs/METHODOLOGY.md', 'Normative benchmark methodology', '7c5cd2e860c6c93047f36ec1b1f69f5a9d9f07008d354455ea85770b5bf385fe'),
    ('analysis/v1-vs-dusk/src/pipeline.rs', 'Seven-stage orchestration', 'e899d3d498d968c3545e1c025013a045cf87a4808061ab47dca877e7dc697ad7'),
    ('programs/dusk/src/benchmark_api.rs', 'Feature-only native transition wrappers', '3f86115819074f77fd9eaf2b1500f3524241ed18573849d38219dbd140684901'),
    ('analysis/v1-vs-dusk/notebooks/benchmark.ipynb', 'Bounded reader-facing notebook', '2802c0d6a37b32f6219290730cfa70f65b326a178f1974e03ffd5e8c29cc9e57'),
    ('analysis/v1-vs-dusk/scripts/execute_notebook.py', 'Restricted notebook validator/executor', '43b5526982b54a69dce67067f1d9c6f54e38cec46493a76445cb45bccd559477'),
    ('analysis/v1-vs-dusk/schemas/catalog.v1.json', 'Strict schema snapshot catalog', '84caa1ba19d4f3bfbb850e1b396654ba24ec298b02ca5f1f35fd86b5d94fa122')
)
SELECT * FROM source_inventory ORDER BY artifact;
