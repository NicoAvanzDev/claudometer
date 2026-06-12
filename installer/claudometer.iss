#define MyAppName "Claudometer"
#define MyAppPublisher "NicoAvanzDev"
#define MyAppExeName "claudometer.exe"
#ifndef MyAppVersion
#define MyAppVersion "1.6.1"
#endif

[Setup]
AppId={{D04B9C64-4E03-4AB2-95DC-3288F67DB3DD}
AppName={#MyAppName}
AppVersion={#MyAppVersion}
AppPublisher={#MyAppPublisher}
DefaultDirName={localappdata}\Programs\{#MyAppName}
DefaultGroupName={#MyAppName}
DisableProgramGroupPage=yes
OutputDir=..\dist
OutputBaseFilename=Claudometer-Setup-{#MyAppVersion}
SetupIconFile=..\assets\icon128x128.ico
UninstallDisplayIcon={app}\{#MyAppExeName}
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
PrivilegesRequired=lowest
CloseApplications=force

[Files]
Source: "..\target\release\{#MyAppExeName}"; DestDir: "{app}"; Flags: ignoreversion; BeforeInstall: StopRunningApp

[Icons]
Name: "{autoprograms}\{#MyAppName}"; Filename: "{app}\{#MyAppExeName}"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "{#MyAppName}"; ValueData: """{app}\{#MyAppExeName}"""; Flags: uninsdeletevalue

[Run]
Filename: "{app}\{#MyAppExeName}"; Description: "Launch {#MyAppName}"; Flags: nowait postinstall skipifsilent

[Code]
procedure StopRunningApp;
var
  ResultCode: Integer;
begin
  if Exec(
    ExpandConstant('{sys}\taskkill.exe'),
    '/IM "{#MyAppExeName}" /T /F',
    '',
    SW_HIDE,
    ewWaitUntilTerminated,
    ResultCode
  ) then begin
    if ResultCode = 0 then
      Log('{#MyAppName} running process stopped before install')
    else
      Log(Format('{#MyAppName} stop command exited with code %d', [ResultCode]));
  end else begin
    Log('{#MyAppName} stop command could not be started');
  end;
end;
