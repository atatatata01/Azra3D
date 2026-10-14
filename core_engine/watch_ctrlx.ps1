$p = Join-Path $PSScriptRoot "ctrlx_diag.log"
$err = Join-Path $PSScriptRoot "ctrlx_stderr.log"
$last = 0
$lastErr = 0
Write-Output "[watch] tailing $p"
while ($true) {
    foreach ($pair in @(@($p, [ref]$last), @($err, [ref]$lastErr))) {
        $file = $pair[0]
        $idx = $pair[1]
        if (Test-Path $file) {
            try {
                $lines = Get-Content -LiteralPath $file -ErrorAction Stop
                $n = @($lines).Count
                if ($n -gt $idx.Value) {
                    for ($i = $idx.Value; $i -lt $n; $i++) {
                        $line = $lines[$i]
                        if ($line -match 'CTRLX|cut_selection|poly_dissolve|REMOVE|BLOCK|refused|ACT_CUT') {
                            Write-Output $line
                        }
                    }
                    $idx.Value = $n
                }
            } catch {}
        }
    }
    Start-Sleep -Milliseconds 500
}
