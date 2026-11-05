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
;
; A new installation (not an upgrade) asks how to start; /MODE= etc. choose silently:
;   Fresh install  an empty Reelix (/MODE=fresh, the default).
;   Recovery       restore a backup zip (/MODE=recovery /BACKUP=<zip>): checked with
;                  `reelix restore check` on its page, staged with `reelix restore stage` after the
;                  files are installed and before the first start, which applies it.
;   Migration      (/MODE=migrate) either from Radarr/Sonarr/Prowlarr/Lidarr/Readarr found on this PC
;                  (their config.xml; /ARR=radarr,sonarr): `reelix migrate prepare` saves URL and
;                  API key (from a temporary file, never the command line) for System > Migrate;
;                  or from another Reelix folder (/MIGRATEFROM=<folder>): its data folder is copied
;                  to the new data folder and `reelix config relocate` fixes paths in config.toml.
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
; Inno's uninstall key for AppId below (keep in sync); present when Reelix is already installed.
#define UninstallKey "Software\Microsoft\Windows\CurrentVersion\Uninstall\{6B0E2D4A-3C71-4F8B-9A52-7E1D0C4B8F23}_is1"

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
const
  ModeFresh = 0;
  ModeRecovery = 1;
  ModeMigrate = 2;
  BackupFilter = 'Reelix backups (reelix_backup_*.zip)|reelix_backup_*.zip|Zip files (*.zip)|*.zip|All files (*.*)|*.*';

var
  ServiceExisted: Boolean;
  NotesPage: TOutputMsgMemoWizardPage;
  ModePage: TInputOptionWizardPage;
  BackupPage: TInputFileWizardPage;
  MigrateKindPage: TInputOptionWizardPage;
  ArrPage: TInputOptionWizardPage;
  ReelixDirPage: TInputDirWizardPage;
  { Reelix is already installed: keep its data, don't ask how to install. }
  IsUpgrade: Boolean;
  { This run created the data folder (so its ACL may be reset after preparing data). }
  DataDirCreated: Boolean;
  { Migration from a Reelix folder, but the user chose to keep the data already there. }
  KeepExistingData: Boolean;
  { *arr apps found on this PC: id (radarr), name (Radarr), URL and API key. }
  ArrIds, ArrNames, ArrUrls, ArrKeys: array of String;
  { What PrepareData did, for the "Getting started" page. }
  PreparedSummary: String;

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

function IcaclsExe: String;
begin
  Result := ExpandConstant('{sys}\icacls.exe');
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

{ Runs reelix.exe (Exe) hidden with Params; its error message (stderr) ends up in Output. }
function RunReelix(const Exe, Params: String; var Output: String): Integer;
var
  ErrFile: String;
  Code: Integer;
  Captured: AnsiString;
begin
  ErrFile := ExpandConstant('{tmp}\reelix-stderr.txt');
  DeleteFile(ErrFile);
  Log('Running: ' + Exe + ' ' + Params);
  { cmd /S /C strips the outer quotes and keeps the rest, quoted paths included. }
  if not Exec(ExpandConstant('{cmd}'), '/S /C ""' + Exe + '" ' + Params + ' 2> "' + ErrFile + '""',
              '', SW_HIDE, ewWaitUntilTerminated, Code) then
    Code := -1;
  Output := '';
  if LoadStringFromFile(ErrFile, Captured) then
    Output := Trim(String(Captured));
  DeleteFile(ErrFile);
  Log('Exit code: ' + IntToStr(Code) + ' ' + Output);
  if (Code <> 0) and (Output = '') then
    Output := 'reelix.exe exited with code ' + IntToStr(Code) + '.';
  Result := Code;
end;

function InstalledExe: String;
begin
  Result := ExpandConstant('{app}\reelix.exe');
end;

{ reelix.exe unpacked to the temporary folder, for checks before anything is installed. }
function TempExe: String;
begin
  Result := ExpandConstant('{tmp}\reelix.exe');
  if not FileExists(Result) then
    ExtractTemporaryFile('reelix.exe');
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

{ ---------- How to install: fresh, recovery or migration ---------- }

function InstallMode: Integer;
begin
  if IsUpgrade then
    Result := ModeFresh
  else
    Result := ModePage.SelectedValueIndex;
end;

function MigrateFromReelix: Boolean;
begin
  Result := MigrateKindPage.SelectedValueIndex = 1;
end;

{ True if the comma-separated List (any case, spaces allowed) contains Item. }
function ListHas(const List, Item: String): Boolean;
var
  L: String;
begin
  L := Lowercase(List);
  StringChangeEx(L, ' ', '', True);
  Result := Pos(',' + Lowercase(Item) + ',', ',' + L + ',') > 0;
end;

{ The text of the first <Tag>...</Tag> in Xml ('' if missing or empty). }
function XmlValue(const Xml, Tag: String): String;
var
  P: Integer;
  Rest: String;
begin
  Result := '';
  P := Pos('<' + Tag + '>', Xml);
  if P = 0 then
    Exit;
  Rest := Copy(Xml, P + Length(Tag) + 2, Length(Xml));
  P := Pos('</' + Tag + '>', Rest);
  if P > 0 then
    Result := Trim(Copy(Rest, 1, P - 1));
end;

{ The *arr's HTTP address from its config.xml. It keeps serving HTTP on <Port> when SSL is on. }
function ArrUrl(const Xml, DefaultPort: String): String;
var
  Host, Port, UrlBase: String;
begin
  Host := XmlValue(Xml, 'BindAddress');
  if (Host = '') or (Host = '*') or (Host = '0.0.0.0') or (Pos(':', Host) > 0) then
    Host := 'localhost';
  Port := XmlValue(Xml, 'Port');
  if StrToIntDef(Port, 0) <= 0 then
    Port := DefaultPort;
  UrlBase := XmlValue(Xml, 'UrlBase');
  if (UrlBase <> '') and (Copy(UrlBase, 1, 1) <> '/') then
    UrlBase := '/' + UrlBase;
  while (Length(UrlBase) > 0) and (Copy(UrlBase, Length(UrlBase), 1) = '/') do
    UrlBase := Copy(UrlBase, 1, Length(UrlBase) - 1);
  Result := 'http://' + Host + ':' + Port + UrlBase;
end;

{ Reads ConfigFile if it exists and has an API key; adds the app to the Arr* lists. }
function TryArrConfig(const ConfigFile, Id, Name, DefaultPort: String): Boolean;
var
  Raw: AnsiString;
  Xml, Key: String;
  N: Integer;
begin
  Result := False;
  if not FileExists(ConfigFile) or not LoadStringFromFile(ConfigFile, Raw) then
    Exit;
  Xml := String(Raw);
  Key := XmlValue(Xml, 'ApiKey');
  if Key = '' then begin
    Log(ConfigFile + ' has no API key; skipped');
    Exit;
  end;
  N := GetArrayLength(ArrIds);
  SetArrayLength(ArrIds, N + 1);
  SetArrayLength(ArrNames, N + 1);
  SetArrayLength(ArrUrls, N + 1);
  SetArrayLength(ArrKeys, N + 1);
  ArrIds[N] := Id;
  ArrNames[N] := Name;
  ArrUrls[N] := ArrUrl(Xml, DefaultPort);
  ArrKeys[N] := Key;
  Log('Found ' + Name + ' (' + ConfigFile + '): ' + ArrUrls[N] + ', EnableSsl=' + XmlValue(Xml, 'EnableSsl') +
    ', SslPort=' + XmlValue(Xml, 'SslPort'));
  Result := True;
end;

{ An *arr on Windows keeps its config in %ProgramData%\<App> (service/tray default), or in the
  AppData of the user running it. }
procedure FindArr(const Id, Name, DefaultPort: String);
begin
  if not TryArrConfig(ExpandConstant('{commonappdata}\') + Name + '\config.xml', Id, Name, DefaultPort) then
    if not TryArrConfig(ExpandConstant('{localappdata}\') + Name + '\config.xml', Id, Name, DefaultPort) then
      TryArrConfig(ExpandConstant('{userappdata}\') + Name + '\config.xml', Id, Name, DefaultPort);
end;

procedure FindArrApps;
begin
  FindArr('radarr', 'Radarr', '7878');
  FindArr('sonarr', 'Sonarr', '8989');
  FindArr('prowlarr', 'Prowlarr', '9696');
  FindArr('lidarr', 'Lidarr', '8686');
  FindArr('readarr', 'Readarr', '8787');
end;

{ The data folder inside an old Reelix folder: <Folder>\data, or Folder itself if the user picked
  the data folder. '' if there is no reelix.db. }
function ReelixDataIn(const Folder: String): String;
var
  F: String;
begin
  Result := '';
  F := RemoveBackslashUnlessRoot(Trim(Folder));
  if F = '' then
    Exit;
  if FileExists(F + '\data\reelix.db') then
    Result := F + '\data'
  else if FileExists(F + '\reelix.db') then
    Result := F;
end;

{ Whether a reelix.exe started from Folder (or below it) is running. }
function OldReelixRunning(const Folder: String): Boolean;
begin
  Result := RunPowerShell('$d = ' + PsQuote(AddBackslash(Folder)) + '; ' +
    'if (Get-Process -Name reelix -ErrorAction SilentlyContinue | Where-Object { $_.Path -and ' +
    '$_.Path.StartsWith($d, [StringComparison]::OrdinalIgnoreCase) }) { exit 3 }') = 3;
end;

function CheckBackup(const Zip: String; var Error: String): Boolean;
begin
  Result := False;
  if (Trim(Zip) = '') or not FileExists(Zip) then
    Error := 'The file "' + Zip + '" does not exist.'
  else
    Result := RunReelix(TempExe, 'restore check "' + Zip + '" --data-dir "' + ExpandConstant('{tmp}\check') + '"', Error) = 0;
end;

{ Problems with the choices of a silent install; '' if there are none. }
function CheckSilentChoices: String;
var
  Err, Src: String;
  I: Integer;
begin
  Result := '';
  if InstallMode = ModeRecovery then begin
    if not CheckBackup(BackupPage.Values[0], Err) then
      Result := 'The backup can''t be restored (/BACKUP): ' + Err;
  end else if (InstallMode = ModeMigrate) and MigrateFromReelix then begin
    Src := ReelixDataIn(ReelixDirPage.Values[0]);
    if Src = '' then
      Result := 'No Reelix data (reelix.db) was found in "' + ReelixDirPage.Values[0] + '" (/MIGRATEFROM).'
    else if CompareText(Src, DataDir) = 0 then
      Result := '/MIGRATEFROM is this installation''s own data folder.'
    else if FileExists(DataDir + '\reelix.db') then
      Result := DataDir + ' already has a Reelix database; Setup does not overwrite it.'
    else if OldReelixRunning(ExtractFileDir(Src)) then
      Result := 'The old Reelix in ' + ExtractFileDir(Src) + ' is still running; stop it first.';
  end else if InstallMode = ModeMigrate then begin
    if GetArrayLength(ArrIds) = 0 then
      Log('/MODE=migrate: no Radarr, Sonarr, Prowlarr, Lidarr or Readarr found on this PC');
    for I := 0 to GetArrayLength(ArrIds) - 1 do
      if ArrPage.Values[I] then
        Log('Will save ' + ArrNames[I] + ' at ' + ArrUrls[I] + ' for migration');
  end;
end;

{ Creates the data folder; in service mode with SYSTEM + Administrators only, since ProgramData's
  inherited ACL would let every local user read the database and secret key. }
procedure EnsureDataDir;
var
  Data: String;
begin
  Data := DataDir;
  if DirExists(Data) then
    Exit;
  if not ForceDirectories(Data) then
    Exit;
  DataDirCreated := True;
  if IsAdminInstallMode then
    RunHidden(IcaclsExe, '"' + Data + '" /inheritance:r /grant:r *S-1-5-18:(OI)(CI)F *S-1-5-32-544:(OI)(CI)F');
end;

{ Service mode, after Setup wrote into the data folder as the installing administrator: files
  reelix.exe restricted to that user (secret.key) must be readable by the service (LocalSystem). }
procedure HardenDataDir;
var
  Data: String;
begin
  Data := DataDir;
  if DataDirCreated then begin
    { A folder Setup created: everything inherits its SYSTEM + Administrators ACL again. }
    RunHidden(IcaclsExe, '"' + Data + '" /reset /T /C /Q');
    RunHidden(IcaclsExe, '"' + Data + '" /inheritance:r /grant:r *S-1-5-18:(OI)(CI)F *S-1-5-32-544:(OI)(CI)F');
  end else if FileExists(Data + '\secret.key') then
    RunHidden(IcaclsExe, '"' + Data + '\secret.key" /grant *S-1-5-18:F *S-1-5-32-544:F');
end;

{ Recovery: stage the backup; Reelix applies it when it starts. }
procedure StageBackup;
var
  Zip, Err: String;
begin
  Zip := BackupPage.Values[0];
  while True do begin
    if RunReelix(InstalledExe, 'restore stage "' + Zip + '" --data-dir "' + DataDir + '"', Err) = 0 then begin
      PreparedSummary := 'The backup ' + ExtractFileName(Zip) + ' is ready: Reelix restores it the first time it ' +
        'starts (the files it replaces are kept in ' + DataDir + '\restore).';
      Exit;
    end;
    if SuppressibleMsgBox('The backup could not be prepared for restoring:' + #13#10#13#10 + Err + #13#10#13#10 +
        'Choose another backup file? Choose No to continue with a fresh, empty Reelix.',
        mbError, MB_YESNO, IDNO) <> IDYES then
      Break;
    if not GetOpenFileName('Choose a Reelix backup', Zip, ExtractFileDir(Zip), BackupFilter, 'zip') then
      Break;
  end;
  Log('No backup staged; continuing as a fresh install');
  PreparedSummary := 'No backup was restored, so Reelix starts empty. You can restore a backup later in ' +
    'System > Backup.';
end;

{ Migration from *arr apps: save each ticked app for System > Migrate. }
procedure SaveArrSources;
var
  I: Integer;
  KeyFile, Err, Saved, Failed: String;
begin
  Saved := '';
  Failed := '';
  for I := 0 to GetArrayLength(ArrIds) - 1 do begin
    if ArrPage.Values[I] then begin
      { The key goes through a file only this user can read, never the command line. }
      KeyFile := ExpandConstant('{tmp}\') + ArrIds[I] + '-key.txt';
      Err := 'could not write a temporary file';
      if SaveStringToFile(KeyFile, '', False) then begin
        RunHidden(IcaclsExe, '"' + KeyFile + '" /inheritance:r /grant:r "' + GetUserNameString + '":F');
        if SaveStringToFile(KeyFile, ArrKeys[I], True) and
           (RunReelix(InstalledExe, 'migrate prepare --source ' + ArrIds[I] + ' --url "' + ArrUrls[I] +
              '" --api-key-file "' + KeyFile + '" --data-dir "' + DataDir + '"', Err) = 0) then
          Saved := Saved + ', ' + ArrNames[I]
        else
          Failed := Failed + #13#10 + ArrNames[I] + ': ' + Err;
        DeleteFile(KeyFile);
      end else
        Failed := Failed + #13#10 + ArrNames[I] + ': ' + Err;
    end;
  end;
  if Saved <> '' then
    PreparedSummary := 'Ready to migrate from ' + Copy(Saved, 3, Length(Saved)) + '. After you create the ' +
      'admin account, Reelix shows "Ready to migrate": System > Migrate is filled in, so you only review the ' +
      'preview and start it. Nothing is imported before you do; the other apps are not changed.';
  if Failed <> '' then
    SuppressibleMsgBox('These apps could not be saved for migration:' + Failed + #13#10#13#10 +
      'You can still migrate from them in System > Migrate by entering the address and API key.',
      mbError, MB_OK, IDOK);
end;

{ Migration from another Reelix folder: copy its data folder to the new one. }
procedure CopyOldReelix;
var
  Src, Data, Err: String;
  Code: Integer;
begin
  Src := ReelixDataIn(ReelixDirPage.Values[0]);
  Data := DataDir;
  if (Src = '') or KeepExistingData or FileExists(Data + '\reelix.db') then begin
    Log('Not copying old Reelix data: none found, or ' + Data + ' already has a database');
    PreparedSummary := 'The existing data in ' + Data + ' was kept; nothing was copied.';
    Exit;
  end;
  while OldReelixRunning(ExtractFileDir(Src)) do begin
    if SuppressibleMsgBox('The old Reelix in ' + ExtractFileDir(Src) + ' is running. Close it and click Retry, ' +
        'or click Cancel to skip copying its data.', mbError, MB_RETRYCANCEL, IDCANCEL) <> IDRETRY then begin
      PreparedSummary := 'The old Reelix data was not copied because it was still running. Stop both, copy ' +
        Src + ' to ' + Data + ' and start Reelix again.';
      Exit;
    end;
  end;
  { /COPY:DAT leaves out the old ACLs: the copies inherit the new data folder's. }
  Code := RunHidden(ExpandConstant('{sys}\robocopy.exe'),
    '"' + Src + '" "' + Data + '" /E /COPY:DAT /DCOPY:T /R:2 /W:1 /NP /NJH /NJS');
  { robocopy: 0-7 is success (1 = files copied), 8 and up means some files failed. }
  if (Code < 0) or (Code >= 8) then begin
    SuppressibleMsgBox('Copying ' + Src + ' to ' + Data + ' failed (robocopy exit code ' + IntToStr(Code) +
      '); see the setup log. Stop Reelix and copy the folder by hand before using it.', mbError, MB_OK, IDOK);
    PreparedSummary := 'Copying the old Reelix data from ' + Src + ' failed; see the setup log.';
    Exit;
  end;
  if RunReelix(InstalledExe, 'config relocate --from "' + Src + '" --data-dir "' + Data + '"', Err) <> 0 then
    SuppressibleMsgBox('config.toml could not be updated for the new data folder:' + #13#10#13#10 +
      Err + #13#10#13#10 + 'Check the paths in ' + Data + '\config.toml.', mbError, MB_OK, IDOK);
  PreparedSummary := 'The data of the old Reelix (' + Src + ') was copied to ' + Data + '. The old folder ' +
    'was left as it is; delete it once everything works here.';
end;

{ Before the first start (and the service registration): the recovery or migration choice. }
procedure PrepareData;
begin
  if InstallMode = ModeFresh then
    Exit;
  EnsureDataDir;
  if InstallMode = ModeRecovery then
    StageBackup
  else if MigrateFromReelix then
    CopyOldReelix
  else
    SaveArrSources;
  if IsAdminInstallMode then
    HardenDataDir;
end;

function PrepareToInstall(var NeedsRestart: Boolean): String;
var
  AppDir: String;
begin
  Result := '';
  { Interactive choices were checked on their pages; check the command line's before installing. }
  if WizardSilent and not IsUpgrade then begin
    Result := CheckSilentChoices;
    if Result <> '' then begin
      Log(Result);
      Exit;
    end;
  end;
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
  EnsureDataDir;

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
    { Before Reelix first starts: the service below, or "Launch Reelix" after this step. }
    PrepareData;
    if IsAdminInstallMode then
      InstallService
    else if not WizardIsTaskSelected('autostart') then
      { Unticked on an upgrade: remove the value an earlier install created. }
      RegDeleteValue(HKEY_CURRENT_USER, 'Software\Microsoft\Windows\CurrentVersion\Run', 'Reelix');
  end;
end;

procedure InitializeWizard;
var
  I, RootKey: Integer;
  Mode, Arr, S: String;
begin
  if IsAdminInstallMode then
    RootKey := HKEY_LOCAL_MACHINE
  else
    RootKey := HKEY_CURRENT_USER;
  IsUpgrade := RegKeyExists(RootKey, '{#UninstallKey}') or (WizardForm.PrevAppDir <> '');
  FindArrApps;

  ModePage := CreateInputOptionPage(wpWelcome, 'Installation type', 'How would you like to install Reelix?',
    'Choose how this installation starts, then click Next.', True, False);
  ModePage.Add('Fresh install: a new, empty Reelix');
  ModePage.Add('Recovery: restore a Reelix backup (a reelix_backup_*.zip from System > Backup)');
  ModePage.Add('Migration: bring over Radarr, Sonarr, Prowlarr, Lidarr, Readarr or an older Reelix');
  ModePage.SelectedValueIndex := ModeFresh;

  BackupPage := CreateInputFilePage(ModePage.ID, 'Recovery', 'Which backup should Reelix restore?',
    'Choose a backup downloaded from System > Backup in Reelix (reelix_backup_*.zip). Setup checks it now; ' +
    'Reelix restores it the first time it starts, including its settings and secret key.');
  BackupPage.Add('Backup file:', BackupFilter, '.zip');

  MigrateKindPage := CreateInputOptionPage(BackupPage.ID, 'Migration', 'What are you migrating from?',
    'Reelix reads the other app; nothing is changed there.', True, False);
  MigrateKindPage.Add('Radarr, Sonarr, Prowlarr, Lidarr or Readarr on this PC');
  MigrateKindPage.Add('An existing Reelix folder (for example the portable zip)');
  MigrateKindPage.SelectedValueIndex := 0;

  if GetArrayLength(ArrIds) = 0 then
    S := 'Setup found no Radarr, Sonarr, Prowlarr, Lidarr or Readarr on this PC (it looks for their ' +
      'config.xml in ProgramData and AppData). Go back and choose another option; you can migrate from ' +
      'apps on other computers later in System > Migrate.'
  else
    S := 'Setup saves the address and API key (encrypted) of the apps you tick. After you create the admin ' +
      'account, System > Migrate shows what Reelix would import; nothing is imported until you start it.';
  ArrPage := CreateInputOptionPage(MigrateKindPage.ID, 'Migration', 'Which apps should Reelix migrate from?',
    S, False, False);
  for I := 0 to GetArrayLength(ArrIds) - 1 do begin
    ArrPage.Add(ArrNames[I] + ' (' + ArrUrls[I] + ')');
    ArrPage.Values[I] := True;
  end;

  ReelixDirPage := CreateInputDirPage(ArrPage.ID, 'Migration', 'Where is the old Reelix?',
    'Choose the folder of the old Reelix: the one with reelix.exe and its "data" folder. Setup copies the ' +
    'data (database, config.toml, secret key, backups) to ' + DataDir + ' and leaves the old folder as it is.',
    False, '');
  ReelixDirPage.Add('Old Reelix folder:');

  NotesPage := CreateOutputMsgMemoPage(wpInfoAfter, 'Getting started',
    'How to finish setting up Reelix.', 'Read this before you continue:', '');

  { Command line: /MODE=fresh|recovery|migrate /BACKUP=<zip> /MIGRATEFROM=<folder> /ARR=radarr,sonarr }
  Mode := Lowercase(ExpandConstant('{param:MODE|}'));
  if Mode = 'recovery' then
    ModePage.SelectedValueIndex := ModeRecovery
  else if (Mode = 'migrate') or (Mode = 'migration') then
    ModePage.SelectedValueIndex := ModeMigrate
  else if (Mode <> '') and (Mode <> 'fresh') then
    Log('Unknown /MODE=' + Mode + '; using a fresh install');
  BackupPage.Values[0] := ExpandConstant('{param:BACKUP|}');
  ReelixDirPage.Values[0] := ExpandConstant('{param:MIGRATEFROM|}');
  if ReelixDirPage.Values[0] <> '' then
    MigrateKindPage.SelectedValueIndex := 1;
  Arr := ExpandConstant('{param:ARR|}');
  if Arr <> '' then begin
    for I := 0 to GetArrayLength(ArrIds) - 1 do
      ArrPage.Values[I] := ListHas(Arr, ArrIds[I]);
    Log('/ARR=' + Arr + '; only apps found on this PC are used');
  end;
  if IsUpgrade then
    Log('Reelix is already installed: upgrading, the existing data is kept');
end;

function ShouldSkipPage(PageID: Integer): Boolean;
begin
  Result := False;
  if (PageID = ModePage.ID) or (PageID = BackupPage.ID) or (PageID = MigrateKindPage.ID) or
     (PageID = ArrPage.ID) or (PageID = ReelixDirPage.ID) then begin
    if IsUpgrade then
      Result := True
    else if PageID = BackupPage.ID then
      Result := InstallMode <> ModeRecovery
    else if PageID = MigrateKindPage.ID then
      Result := InstallMode <> ModeMigrate
    else if PageID = ArrPage.ID then
      Result := (InstallMode <> ModeMigrate) or MigrateFromReelix
    else if PageID = ReelixDirPage.ID then
      Result := (InstallMode <> ModeMigrate) or not MigrateFromReelix;
  end;
end;

function NextButtonClick(CurPageID: Integer): Boolean;
var
  Err, Src: String;
  I: Integer;
  AnyTicked: Boolean;
begin
  Result := True;
  if WizardSilent then
    Exit;
  if CurPageID = BackupPage.ID then begin
    if not CheckBackup(BackupPage.Values[0], Err) then begin
      MsgBox('This file can''t be restored:' + #13#10#13#10 + Err, mbError, MB_OK);
      Result := False;
    end;
  end else if CurPageID = ArrPage.ID then begin
    AnyTicked := False;
    for I := 0 to GetArrayLength(ArrIds) - 1 do
      if ArrPage.Values[I] then
        AnyTicked := True;
    if not AnyTicked then begin
      if GetArrayLength(ArrIds) = 0 then
        MsgBox('No apps to migrate from were found on this PC. Click Back to choose another option.', mbInformation, MB_OK)
      else
        MsgBox('Tick at least one app, or click Back to choose another option.', mbInformation, MB_OK);
      Result := False;
    end;
  end else if CurPageID = ReelixDirPage.ID then begin
    KeepExistingData := False;
    Src := ReelixDataIn(ReelixDirPage.Values[0]);
    if Src = '' then begin
      MsgBox('No Reelix data was found in "' + ReelixDirPage.Values[0] + '".' + #13#10#13#10 +
        'Choose the folder that contains reelix.exe and the "data" folder with reelix.db (or the data folder itself).',
        mbError, MB_OK);
      Result := False;
      Exit;
    end;
    if CompareText(Src, DataDir) = 0 then begin
      MsgBox('That is the data folder of this installation already. Choose the old Reelix folder.', mbError, MB_OK);
      Result := False;
      Exit;
    end;
    if FileExists(DataDir + '\reelix.db') then begin
      if MsgBox(DataDir + ' already has a Reelix database, and Setup does not overwrite it.' + #13#10#13#10 +
          'Keep the existing data and skip copying? Choose No to pick another option.',
          mbConfirmation, MB_YESNO) = IDYES then
        KeepExistingData := True
      else begin
        Result := False;
        Exit;
      end;
    end;
    if not KeepExistingData then
      while OldReelixRunning(ExtractFileDir(Src)) do begin
        if MsgBox('The old Reelix in ' + ExtractFileDir(Src) + ' is running. Close it (its console window, or ' +
            'however you started it), then click Retry.', mbError, MB_RETRYCANCEL) <> IDRETRY then begin
          Result := False;
          Exit;
        end;
      end;
  end;
end;

procedure CurPageChanged(CurPageID: Integer);
var
  S: String;
begin
  if CurPageID <> NotesPage.ID then
    Exit;
  S := '';
  if PreparedSummary <> '' then
    S := PreparedSummary + #13#10#13#10;
  if IsAdminInstallMode then
    S := S + 'Reelix is installed as the Windows service "Reelix". It starts automatically with Windows, ' +
      'also when nobody is signed in, and restarts itself after updates.' + #13#10#13#10
  else
    S := S + 'Reelix runs while you are signed in. Start it from the Start menu ("Reelix"): it opens a ' +
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
