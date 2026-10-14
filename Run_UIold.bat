@echo off
REM ─────────────────────────────────────────────────────────────
REM  AZRA — launch the OLD UI (default) on the current branch.
REM  AZRA_UI is left unset, so the old chrome draws (unchanged).
REM  Use this to compare against the new UI (Run_UIv2.bat).
REM ─────────────────────────────────────────────────────────────
cd /d "%~dp0core_engine"
echo [AZRA] branch:
git -C "%~dp0" rev-parse --abbrev-ref HEAD
echo [AZRA] launching OLD UI...
set AZRA_UI=
cargo run -p app
echo.
echo [AZRA] app exited.
pause
