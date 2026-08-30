#!/usr/bin/env bash
set -euo pipefail

# These targets exercise subprocesses, three-path semantic comparison, package
# construction, or large generated language matrices. Run them in CI and once
# at final acceptance, not after every narrow edit.
slow_targets=(
  binding_matrix
  call_matrix
  cli_evidence_observation
  cli_pack
  cli_project
  cli_resource
  cli_run
  cli_schedule
  cli_secret
  cli_test
  completion_matrix
  concurrency_matrix
  final_review
  gui_cli_hosts
  language_matrix
  module_matrix
  new_semantics_gate
  privacy_propagation
  semantic_gate
  statement_matrix
  type_matrix
  unit_semantics
  value_matrix
)

for target in "${slow_targets[@]}"; do
  cargo test --test "$target"
done
