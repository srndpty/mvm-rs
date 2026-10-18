@echo off
pwsh -NoProfile -File "%~dp0dev.ps1" %*
exit /b %ERRORLEVEL%
