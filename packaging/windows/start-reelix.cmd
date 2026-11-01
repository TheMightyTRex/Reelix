@echo off
rem Starts Reelix from this folder; settings and the database go in .\data.
cd /d "%~dp0"
echo Reelix is starting on http://localhost:7700  (close this window to stop it)
reelix.exe --data-dir "%~dp0data" %*
if errorlevel 1 pause
