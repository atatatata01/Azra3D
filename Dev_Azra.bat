@echo off
setlocal

:: ── Claude API Key (optional) ──
:: set ANTHROPIC_API_KEY=sk-ant-...

cd /d "F:\PROJECT\FourthLAyerAi\core_engine"
echo Building...
cargo build -p app
if %ERRORLEVEL% == 0 (
    echo Launching...
    target\debug\app.exe %*
) else (
    echo BUILD FAILED.
)
pause
