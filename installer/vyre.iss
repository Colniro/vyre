; Vyre-Setup (Inno Setup 6). Gebaut von scripts\release.ps1, das die Version mitgibt:
;   ISCC.exe /DAppVersion=0.14.0 installer\vyre.iss
; Installiert für den aktuellen Benutzer (ohne Adminrechte): %LOCALAPPDATA%\Programs\Vyre.

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif

[Setup]
AppId={{C4E1A7D2-8B3F-4F6A-9E52-3D7B1A0C6E94}
AppName=Vyre
AppVersion={#AppVersion}
AppVerName=Vyre {#AppVersion}
AppPublisher=Vyre
AppPublisherURL=https://github.com/Colniro/vyre
VersionInfoVersion={#AppVersion}
DefaultDirName={localappdata}\Programs\Vyre
DefaultGroupName=Vyre
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
OutputDir=..\dist
OutputBaseFilename=vyre-setup-{#AppVersion}
UninstallDisplayIcon={app}\vyre.exe
LicenseFile=..\LICENSE-MIT
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; Das Update läuft, während Vyre offen ist: schließen und danach wieder starten.
CloseApplications=yes
RestartApplications=yes
ChangesEnvironment=yes

[Languages]
Name: "german"; MessagesFile: "compiler:Languages\German.isl"
Name: "english"; MessagesFile: "compiler:Default.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
Name: "addtopath"; Description: "Befehl ""vyre"" im Terminal (PATH)"; GroupDescription: "Terminal:"

[Files]
Source: "..\target\release\vyre.exe"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\README.md"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE-MIT"; DestDir: "{app}"; Flags: ignoreversion
Source: "..\LICENSE-APACHE"; DestDir: "{app}"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Vyre"; Filename: "{app}\vyre.exe"
Name: "{autoprograms}\Vyre (privates Fenster)"; Filename: "{app}\vyre.exe"; Parameters: "--privat"
Name: "{autodesktop}\Vyre"; Filename: "{app}\vyre.exe"; Tasks: desktopicon

[Registry]
; "vyre" im Terminal (vyre open, tabs, reload, konsole, update).
Root: HKCU; Subkey: "Environment"; ValueType: expandsz; ValueName: "Path"; ValueData: "{olddata};{app}"; Tasks: addtopath; Check: NeedsAddPath(ExpandConstant('{app}'))

[Run]
Filename: "{app}\vyre.exe"; Description: "{cm:LaunchProgram,Vyre}"; Flags: nowait postinstall skipifsilent
; Update aus Vyre heraus (still, mit /RESTARTAPPLICATIONS): Vyre hat sich beendet, wieder starten.
Filename: "{app}\vyre.exe"; Flags: nowait runasoriginaluser; Check: IsInAppUpdate

[Code]
function IsInAppUpdate: Boolean;
begin
  Result := WizardSilent and (Pos('/RESTARTAPPLICATIONS', Uppercase(GetCmdTail)) > 0);
end;

function NeedsAddPath(Dir: string): Boolean;
var
  Path: string;
begin
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Path) then
  begin
    Result := True;
    exit;
  end;
  Result := Pos(';' + Uppercase(Dir) + ';', ';' + Uppercase(Path) + ';') = 0;
end;

{ Deinstallieren: Vyre wieder aus dem PATH nehmen (Profil in %LOCALAPPDATA%\Vyre bleibt). }
procedure CurUninstallStepChanged(CurUninstallStep: TUninstallStep);
var
  Path, Dir: string;
  P: Integer;
begin
  if CurUninstallStep <> usPostUninstall then
    exit;
  if not RegQueryStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Path) then
    exit;
  Dir := ExpandConstant('{app}');
  P := Pos(';' + Uppercase(Dir), Uppercase(Path));
  if P > 0 then
  begin
    Delete(Path, P, Length(Dir) + 1);
    RegWriteExpandStringValue(HKEY_CURRENT_USER, 'Environment', 'Path', Path);
  end;
end;
