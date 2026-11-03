; Inno Setup 6 script for the Reelix Windows installer.
;
;   iscc /DAppVersion=0.7.0 /DSourceDir=C:\path\to\reelix-0.7.0-windows-x86_64 /Odist packaging\windows\reelix.iss
;
; SourceDir is the unpacked Windows release zip (reelix.exe, web\, docs\, LICENSE.txt, ...).
; The output is reelix-<version>-windows-x86_64-setup.exe.
;
; One installer, two modes (Setup asks; /CURRENTUSER or /ALLUSERS choose on the command line):
;   Install for me only   no admin rights. Program in %LOCALAPPDATA%\Programs\Reelix, data in
;                         %LOCALAPPDATA%\Reelix. Runs while the user is signed in (Start menu
;                         shortcut, optional sign-in autostart). install.json kind "windows-user".
;   Install for all users needs admin. Program in C:\Program Files\Reelix, data in
;                         C:\ProgramData\Reelix. Runs as the Windows service "Reelix" (LocalSystem),
;                         registered by `reelix.exe service install`. Kind "windows-service".
;
; Stopping a running Reelix before upgrade/uninstall: per-user mode kills only the reelix.exe
; whose image path is this install's {app}\reelix.exe (PowerShell Get-Process + Path filter;
; Reelix has no window or mutex to close it gracefully, and SQLite survives a hard stop).
; Service mode stops the "Reelix" service through the SCM and waits up to 30 s.
; See docs/installers.md.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\..\dist\reelix-" + AppVersion + "-windows-x86_64"
#endif
; VersionInfoVersion must be numeric: 0.7.0-rc.1 -> 0.7.0.
#define NumericVersion Copy(AppVersion, 1, Pos("-", AppVersion + "-") - 1)
#define AppUrl "https://github.com/demonzoned/Reelix"
#define WebUrl "http://localhost:7700"
#define ServiceName "Reelix"

[Setup]
; Fixed AppId: every version upgrades the same installation in place. Never change it.
AppId={{6B0E2D4A-3C71-4F8B-9A52-7E1D0C4B8F23}
AppName=Reelix
AppVersion={#AppVersion}
AppVerName=Reelix {#AppVersion}
AppPublisher=Reelix
AppPublisherURL={#AppUrl}
AppSupportURL={#AppUrl}/issues
AppUpdatesURL={#AppUrl}/releases
VersionInfoVersion={#NumericVersion}
VersionInfoProductName=Reelix
VersionInfoDescription=Reelix Setup
; {autopf} is %LOCALAPPDATA%\Programs in per-user mode and C:\Program Files in all-users mode.
DefaultDirName={autopf}\Reelix
DefaultGroupName=Reelix
DisableProgramGroupPage=yes
DisableDirPage=auto
; Ask "Install for me only" (no elevation) vs "Install for all users" (elevates). An upgrade
; reuses the mode of the existing installation (UsePreviousPrivileges defaults to yes).
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog commandline
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
LicenseFile={#SourceDir}\LICENSE.txt
OutputDir=Output
OutputBaseFilename=reelix-{#AppVersion}-windows-x86_64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
SetupLogging=yes
UninstallDisplayIcon={app}\reelix.exe
UninstallDisplayName=Reelix
; Running copies are stopped in PrepareToInstall; Restart Manager is only a fallback.
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "autostart"; Description: "Start Reelix when I sign in"; Check: not IsAdminInstallMode
Name: "firewall"; Description: "Allow other devices on the network to reach Reelix (Windows Firewall rule)"; Check: IsAdminInstallMode
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[InstallDelete]
; The web UI's file names change between versions; drop the old ones.
Type: filesandordirs; Name: "{app}\web"
Type: filesandordirs; Name: "{app}\docs"

[Files]
; install.json (the zip's says "portable") is written by WriteInstallMarker instead.
; start-reelix.cmd keeps its data next to the program, which isn't what an installed copy wants.
Source: "{#SourceDir}\reelix.exe"; DestDir: "{app}"; Flags: ignoreversion; AfterInstall: WriteInstallMarker
Source: "{#SourceDir}\*"; Excludes: "\reelix.exe,\install.json,\start-reelix.cmd"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs

[Icons]
; Per-user: a launcher (console window with the log; closing it stops Reelix).
Name: "{autoprograms}\Reelix"; Filename: "{app}\reelix.exe"; Parameters: "--data-dir ""{localappdata}\Reelix"""; WorkingDir: "{app}"; Comment: "Start Reelix"; Check: not IsAdminInstallMode
Name: "{autodesktop}\Reelix"; Filename: "{app}\reelix.exe"; Parameters: "--data-dir ""{localappdata}\Reelix"""; WorkingDir: "{app}"; Comment: "Start Reelix"; Tasks: desktopicon; Check: not IsAdminInstallMode
; Both modes: open the web UI.
Name: "{autoprograms}\Open Reelix"; Filename: "{#WebUrl}"
Name: "{autodesktop}\Open Reelix"; Filename: "{#WebUrl}"; Tasks: desktopicon; Check: IsAdminInstallMode

[Registry]
; Per-user autostart. `start /d` sets the working directory (the web UI is loaded from .\web)
; and /min keeps the console window out of the way.
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Reelix"; ValueData: """{cmd}"" /c start ""Reelix"" /min /d ""{app}"" ""{app}\reelix.exe"" --data-dir ""{localappdata}\Reelix"""; Tasks: autostart; Flags: uninsdeletevalue; Check: not IsAdminInstallMode

[Run]
; Service registration happens in CurStepChanged so failures can be reported.
Filename: "{app}\reelix.exe"; Parameters: "--data-dir ""{localappdata}\Reelix"""; WorkingDir: "{app}"; Description: "Launch Reelix"; Flags: nowait postinstall skipifsilent; Check: not IsAdminInstallMode
Filename: "{sys}\WindowsPowerShell\v1.0\powershell.exe"; Parameters: "-NoProfile -NonInteractive -Command ""Start-Sleep -Seconds 2; Start-Process '{#WebUrl}'"""; Description: "Open Reelix in the browser"; Flags: runhidden nowait postinstall skipifsilent

[UninstallRun]
Filename: "{app}\reelix.exe"; Parameters: "service uninstall"; WorkingDir: "{app}"; Flags: runhidden waituntilterminated; Check: IsAdminInstallMode; RunOnceId: "ReelixServiceUninstall"
Filename: "{sys}\netsh.exe"; Parameters: "advfirewall firewall delete rule name=""Reelix"""; Flags: runhidden waituntilterminated; Check: IsAdminInstallMode; RunOnceId: "ReelixFirewallDelete"

[UninstallDelete]
; Files Reelix's self-update may have replaced or added (data lives elsewhere and is kept).
Type: filesandordirs; Name: "{app}\web"
Type: filesandordirs; Name: "{app}\docs"
Type: files; Name: "{app}\install.json"
Type: files; Name: "{app}\reelix.exe*"
Type: files; Name: "{app}\start-reelix.cmd"
Type: dirifempty; Name: "{app}"

[Code]
var
  ServiceExisted: Boolean;
  NotesPage: TOutputMsgMemoWizardPage;

function DataDir: String;
begin
  if IsAdminInstallMode then
    Result := ExpandConstant('{commonappdata}\Reelix')
  else
    Result := ExpandConstant('{localappdata}\Reelix');
end;

function PowerShellExe: String;
begin
  Result := ExpandConstant('{sys}\WindowsPowerShell\v1.0\powershell.exe');
end;

{ Runs a PowerShell command hidden; Script must not contain double quotes. }
function RunPowerShell(const Script: String): Integer;
var
  Code: Integer;
begin
  Log('PowerShell: ' + Script);
  if not Exec(PowerShellExe, '-NoProfile -NonInteractive -ExecutionPolicy Bypass -Command "' + Script + '"',
              '', SW_HIDE, ewWaitUntilTerminated, Code) then
    Code := -1;
  Log('PowerShell exit code: ' + IntToStr(Code));
  Result := Code;
end;

function RunHiddenIn(const FileName, Params, WorkDir: String): Integer;
var
  Code: Integer;
begin
  Log('Running: ' + FileName + ' ' + Params);
  if not Exec(FileName, Params, WorkDir, SW_HIDE, ewWaitUntilTerminated, Code) then
    Code := -1;
  Log('Exit code: ' + IntToStr(Code));
  Result := Code;
end;

function RunHidden(const FileName, Params: String): Integer;
begin
  Result := RunHiddenIn(FileName, Params, '');
end;

function PsQuote(const S: String): String;
begin
  Result := S;
  StringChangeEx(Result, '''', '''''', True);
  Result := '''' + Result + '''';
end;

function ServiceExists: Boolean;
begin
  { sc query exits 1060 when the service doesn't exist. }
  Result := RunHidden(ExpandConstant('{sys}\sc.exe'), 'query {#ServiceName}') = 0;
end;

{ Stops reelix.exe processes started from this install's folder only. }
procedure StopInstalledProcess(const AppDir: String);
begin
  RunPowerShell('$p = ' + PsQuote(AddBackslash(AppDir) + 'reelix.exe') + '; ' +
    'Get-Process -Name reelix -ErrorAction SilentlyContinue | Where-Object { $_.Path -eq $p } | ' +
    'ForEach-Object { Stop-Process -Id $_.Id -Force; $_.WaitForExit(15000) | Out-Null }');
end;

procedure StopService(const AppDir: String);
begin
  RunPowerShell('$s = Get-Service -Name {#ServiceName} -ErrorAction SilentlyContinue; ' +
    'if ($s -and $s.Status -ne ''Stopped'') { Stop-Service -Name {#ServiceName} -Force -NoWait -ErrorAction SilentlyContinue; ' +
    'try { $s.WaitForStatus(''Stopped'', [TimeSpan]::FromSeconds(30)) } catch {} }');
  { If it didn't stop in time, make sure the files aren't locked. }
  StopInstalledProcess(AppDir);
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  AppDir: String;
begin
  Result := '';
  AppDir := ExpandConstant('{app}');
  if IsAdminInstallMode then begin
    ServiceExisted := ServiceExists;
    if ServiceExisted then
      StopService(AppDir);
  end else
    StopInstalledProcess(AppDir);
end;

procedure WriteInstallMarker;
var
  Kind: String;
begin
  if IsAdminInstallMode then
    Kind := 'windows-service'
  else
    Kind := 'windows-user';
  if not SaveStringToFile(ExpandConstant('{app}\install.json'), '{"kind": "' + Kind + '"}' + #13#10, False) then
    RaiseException('Could not write ' + ExpandConstant('{app}\install.json'));
end;

procedure InstallService;
var
  Code: Integer;
  Data: String;
begin
  Data := DataDir;
  { A new data folder gets SYSTEM + Administrators only: ProgramData's inherited ACL would
    let every local user read the database and secret key. }
  if not DirExists(Data) then begin
    if ForceDirectories(Data) then
      RunHidden(ExpandConstant('{sys}\icacls.exe'),
        '"' + Data + '" /inheritance:r /grant:r *S-1-5-18:(OI)(CI)F *S-1-5-32-544:(OI)(CI)F');
  end;

  RunHidden(ExpandConstant('{sys}\netsh.exe'), 'advfirewall firewall delete rule name="Reelix"');
  if WizardIsTaskSelected('firewall') then
    RunHidden(ExpandConstant('{sys}\netsh.exe'),
      'advfirewall firewall add rule name="Reelix" dir=in action=allow enable=yes profile=private,domain program="' +
      ExpandConstant('{app}\reelix.exe') + '"');

  if ServiceExisted then
    { Same path and arguments as before; just start the new binary. }
    Code := RunHidden(ExpandConstant('{sys}\sc.exe'), 'start {#ServiceName}')
  else
    Code := RunHiddenIn(ExpandConstant('{app}\reelix.exe'), 'service install --data-dir "' + Data + '"', ExpandConstant('{app}'));
  if Code <> 0 then
    SuppressibleMsgBox('The Reelix service could not be registered or started (exit code ' + IntToStr(Code) + ').' + #13#10#13#10 +
      'Check the setup log and Event Viewer, or run "reelix.exe service install --data-dir ""' + Data +
      '""" from an administrator command prompt in ' + ExpandConstant('{app}') + '.',
      mbError, MB_OK, IDOK);
end;

procedure CurStepChanged(CurStep: TSetupStep);
begin
  if CurStep = ssPostInstall then begin
    if IsAdminInstallMode then
      InstallService
    else if not WizardIsTaskSelected('autostart') then
      { Unticked on an upgrade: remove the value an earlier install created. }
      RegDeleteValue(HKEY_CURRENT_USER, 'Software\Microsoft\Windows\CurrentVersion\Run', 'Reelix');
  end;
end;

procedure InitializeWizard;
begin
  NotesPage := CreateOutputMsgMemoPage(wpInfoAfter, 'Getting started',
    'How to finish setting up Reelix.', 'Read this before you continue:', '');
end;

procedure CurPageChanged(CurPageID: Integer);
var
  S: String;
begin
  if CurPageID <> NotesPage.ID then
    Exit;
  if IsAdminInstallMode then
    S := 'Reelix is installed as the Windows service "Reelix". It starts automatically with Windows, ' +
      'also when nobody is signed in, and restarts itself after updates.' + #13#10#13#10
  else
    S := 'Reelix runs while you are signed in. Start it from the Start menu ("Reelix"): it opens a ' +
      'console window with its log, and closing that window stops Reelix.' + #13#10#13#10;
  S := S + 'Open {#WebUrl} (Start menu: "Open Reelix") and complete the setup wizard.' + #13#10#13#10 +
    'First run: setting up from this PC needs no token. If you open the setup page from another ' +
    'device, Reelix asks for a one-time setup token. It prints the token to its ';
  if IsAdminInstallMode then
    S := S + 'log in ' + DataDir + '\logs (readable by administrators).'
  else
    S := S + 'console window and to the log in ' + DataDir + '\logs.';
  S := S + #13#10#13#10 + 'Settings, the database, backups and logs are stored in ' + DataDir +
    '. Uninstalling Reelix keeps this folder unless you choose to delete it.' + #13#10#13#10 +
    'Updates: System > Updates in Reelix installs new versions in place.' + #13#10#13#10 +
    'Install FFmpeg (ffprobe) and put it on PATH so Reelix can read codec and resolution details.';
  NotesPage.RichEditViewer.Text := S;
end;

procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Data: String;
begin
  if CurUninstallStep = usUninstall then begin
    { Service mode: [UninstallRun] stops and deletes the service. }
    if not IsAdminInstallMode then
      StopInstalledProcess(ExpandConstant('{app}'));
  end else if CurUninstallStep = usPostUninstall then begin
    Data := DataDir;
    if DirExists(Data) then begin
      if SuppressibleMsgBox('Reelix was removed. Your settings, database, backups and logs are still in' + #13#10 +
          Data + #13#10#13#10 + 'Delete them too? Choose No to keep them for a later reinstall.',
          mbConfirmation, MB_YESNO or MB_DEFBUTTON2, IDNO) = IDYES then
        DelTree(Data, True, True, True);
    end;
  end;
end;
