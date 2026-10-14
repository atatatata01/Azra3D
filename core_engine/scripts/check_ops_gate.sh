#!/usr/bin/env bash
# Ops integrity gate — see check_ops_gate.ps1
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP="$ROOT/app/src"

# Forbidden in UI/dispatch layers:
#   add_primitive_immediate(  and  create_poly_mesh(
# Allowed under ops/, creation.rs, create_tool.rs, poly/, etc.

mapfile -t HITS < <(grep -rnE 'add_primitive_immediate\s*\(|create_poly_mesh\s*\(' "$APP" \
  --include='*.rs' \
  | grep -vE '/ops/|/poly/|creation\.rs|create_tool\.rs|clip_curve_tool\.rs|draw_create\.rs|dev_tools\.rs|project_new\.rs|stress_test\.rs|lab_mesh_sdf\.rs|convert\.rs|instance_actions\.rs' \
  || true)

if [[ ${#HITS[@]} -gt 0 && -n "${HITS[0]:-}" ]]; then
  echo "OPS GATE FAILED — call ops::* instead of create leaves from UI:"
  printf '  %s\n' "${HITS[@]}"
  exit 1
fi
echo "OPS GATE OK"
exit 0
