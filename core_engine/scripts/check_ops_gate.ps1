$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
$appSrc = Join-Path $root "app\src"

function Test-AllowedPath([string]$fullPath) {
    $n = $fullPath.Replace("/", "\")
    if ($n -match "\\ops\\") { return $true }
    if ($n -match "\\poly\\") { return $true }
    $leaf = [IO.Path]::GetFileName($n)
    $allowFiles = @(
        "creation.rs", "create_tool.rs", "clip_curve_tool.rs", "draw_create.rs",
        "dev_tools.rs", "project_new.rs", "stress_test.rs", "lab_mesh_sdf.rs",
        "convert.rs", "instance_actions.rs"
    )
    foreach ($f in $allowFiles) {
        if ($leaf -eq $f) { return $true }
    }
    return $false
}

$violations = New-Object System.Collections.Generic.List[string]
Get-ChildItem -Path $appSrc -Recurse -Filter "*.rs" | ForEach-Object {
    if (Test-AllowedPath $_.FullName) { return }
    $i = 0
    foreach ($line in Get-Content $_.FullName) {
        $i++
        if ($line -match "add_primitive_immediate\s*\(" -or $line -match "create_poly_mesh\s*\(") {
            $rel = $_.FullName.Substring($root.Length).TrimStart("\", "/")
            [void]$violations.Add("${rel}:${i}: $($line.Trim())")
        }
    }
}

if ($violations.Count -gt 0) {
    Write-Host "OPS GATE FAILED - use crate::ops::* for create from UI:"
    foreach ($v in $violations) { Write-Host "  $v" }
    Write-Host "Allowed: ops/**, creation.rs, create_tool.rs, poly/**, ..."
    exit 1
}

Write-Host "OPS GATE OK - no forbidden create leaves outside allowed paths."
exit 0
