; Installs Solo for the current user from the folder script\package-solo.ps1
; stages (dist\Solo), with a Start menu entry, an optional desktop icon, and an
; entry in Settings > Apps to uninstall it.
;
;   ISCC.exe /DAppVersion=<version> /DSourceDir=<dist\Solo> /DOutputDir=<dist> script\solo.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\dist\Solo"
#endif
#ifndef OutputDir
  #define OutputDir "..\dist"
#endif
#define ResourcesDir "..\crates\zed\resources\windows"

[Setup]
AppId={{7712B0F9-D2CA-4827-9E8F-08B7B0D99EEC}
AppName=Solo
AppVersion={#AppVersion}
AppPublisher=Solo
AppPublisherURL=https://github.com/lucas9801/solo
DefaultDirName={localappdata}\Programs\Solo
DefaultGroupName=Solo
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
; The single-instance mutex of a stable Solo (release_channel::app_identifier).
AppMutex=Solo-Terminal-Stable-Instance-Mutex
CloseApplications=force
SetupIconFile={#ResourcesDir}\app-icon.ico
UninstallDisplayIcon={app}\solo.exe
UninstallDisplayName=Solo
WizardStyle=modern
Compression=lzma2/max
SolidCompression=yes
OutputDir={#OutputDir}
OutputBaseFilename=Solo-Setup-{#AppVersion}

[Languages]
Name: "english"; MessagesFile: "compiler:Default.isl"
Name: "chinese"; MessagesFile: "{#ResourcesDir}\messages\Default.zh-cn.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Excludes: "install.ps1"; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{autoprograms}\Solo"; Filename: "{app}\solo.exe"
Name: "{autodesktop}\Solo"; Filename: "{app}\solo.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\solo.exe"; Description: "{cm:LaunchProgram,Solo}"; Flags: nowait postinstall skipifsilent
; Solo installs its updates silently, then starts again, unless it was
; updated as the user quit it (/NOLAUNCH).
Filename: "{app}\solo.exe"; Flags: nowait; Check: ShouldRelaunch

[Code]
function ShouldRelaunch: Boolean;
var
  I: Integer;
begin
  Result := WizardSilent;
  for I := 1 to ParamCount do
    if CompareText(ParamStr(I), '/NOLAUNCH') = 0 then
      Result := False;
end;
