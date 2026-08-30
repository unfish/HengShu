#!/usr/bin/env bash
set -euo pipefail

# Unit tests and integration targets that do not repeatedly build or launch
# standalone packages. Keep this list explicit so new slow tests are classified
# deliberately instead of silently entering the local feedback loop.
cargo test --lib

fast_targets=(
  automation_schedule
  editor_extension
  endpoint_policy
  module_layering
  notebook_observation
  provenance_nodes
  public_api
  reconciliation_properties
  role_recipes
  vscode_notebook_schema
  web_scraping
)

for target in "${fast_targets[@]}"; do
  cargo test --test "$target"
done
