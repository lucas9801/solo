; Installs Solo for the current user from the folder script\package-solo.ps1
; stages (dist\Solo), with a Start menu entry, an optional desktop icon, a
; folder the user can choose, "Open with Solo" in Explorer's context menu,
; solo:// links and an entry in Settings > Apps to uninstall it. Released on
; GitHub as Solo-Setup-<version>.exe, next to Solo Code in the Microsoft
; Store; Solo installed with it updates itself with the next release's
; installer (crates\auto_update\src\solo_update.rs, run silently by
; solo-update-helper.exe).
;
;   ISCC.exe /DAppVersion=<version> /DSourceDir=<dist\Solo> /DOutputDir=<dist> script\solo.iss
;
; (script\package-solo.ps1 -Step Installer does this.) The installer isn't
; signed, so Windows' SmartScreen warns about it when it's downloaded with a
; browser ("More info", then "Run anyway").
;
; The AppId and the AppMutex are those of every earlier Solo installer, so
; installing over an earlier Solo (2.4.2 and before) updates it in its folder.
#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceDir
  #define SourceDir "..\dist\Solo"
#endif
#ifndef OutputDir
  #define OutputDir "..\dist"
#endif
#define ResourcesDir "..\crates\solo\resources\windows"

[Setup]
AppId={{7712B0F9-D2CA-4827-9E8F-08B7B0D99EEC}
AppName=Solo
AppVersion={#AppVersion}
AppPublisher=Solo
AppPublisherURL=https://github.com/lucas9801/solo
; The user's own folder by default; the folder page lets them pick another
; one they can write to (another drive, say) on the first install, and an
; update goes into the folder of the installed Solo (DisableDirPage=auto,
; UsePreviousAppDir).
DefaultDirName={localappdata}\Programs\Solo
DisableDirPage=auto
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

[CustomMessages]
english.AddContextMenu=Add "Open with Solo" to the Explorer context menu
chinese.AddContextMenu=在资源管理器右键菜单中添加“通过 Solo 打开”
english.OpenWithSolo=Open with Solo
chinese.OpenWithSolo=通过 Solo 打开
english.OtherTasks=Other:
chinese.OtherTasks=其他：

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked
; Checked by default. An update (silent, UsePreviousTasks) keeps what the
; user chose last time, and a task the last install didn't have yet gets
; its default, so updating from an older Solo adds the menu.
Name: "contextmenu"; Description: "{cm:AddContextMenu}"; GroupDescription: "{cm:OtherTasks}"

[Files]
Source: "{#SourceDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs

[Icons]
Name: "{autoprograms}\Solo"; Filename: "{app}\solo.exe"
Name: "{autodesktop}\Solo"; Filename: "{app}\solo.exe"; Tasks: desktopicon

; Solo's own links (solo://settings/..., solo://skill?...), for this user;
; removed on uninstall. zed:// is left to Zed.
[Registry]
Root: HKCU; Subkey: "Software\Classes\solo"; ValueType: string; ValueName: ""; ValueData: "URL:Solo"; Flags: uninsdeletekey
Root: HKCU; Subkey: "Software\Classes\solo"; ValueType: string; ValueName: "URL Protocol"; ValueData: ""
Root: HKCU; Subkey: "Software\Classes\solo\DefaultIcon"; ValueType: string; ValueName: ""; ValueData: "{app}\solo.exe,0"
Root: HKCU; Subkey: "Software\Classes\solo\shell\open\command"; ValueType: string; ValueName: ""; ValueData: """{app}\solo.exe"" ""%1"""

; "Open with Solo" in Explorer's context menu (on Windows 11 under "Show more
; options"): on files, folders, a folder's background and drives, for this
; user, like other editors do. Removed on uninstall, and when the task is
; unchecked on a later install.
Root: HKCU; Subkey: "Software\Classes\*\shell\Solo"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenWithSolo}"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\*\shell\Solo"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\solo.exe"; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\*\shell\Solo\command"; ValueType: string; ValueName: ""; ValueData: """{app}\solo.exe"" ""%1"""; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Solo"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenWithSolo}"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Solo"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\solo.exe"; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Solo\command"; ValueType: string; ValueName: ""; ValueData: """{app}\solo.exe"" ""%V"""; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\Solo"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenWithSolo}"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\Solo"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\solo.exe"; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\Solo\command"; ValueType: string; ValueName: ""; ValueData: """{app}\solo.exe"" ""%V"""; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Drive\shell\Solo"; ValueType: string; ValueName: ""; ValueData: "{cm:OpenWithSolo}"; Flags: uninsdeletekey; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Drive\shell\Solo"; ValueType: string; ValueName: "Icon"; ValueData: "{app}\solo.exe"; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\Drive\shell\Solo\command"; ValueType: string; ValueName: ""; ValueData: """{app}\solo.exe"" ""%V"""; Tasks: contextmenu
Root: HKCU; Subkey: "Software\Classes\*\shell\Solo"; ValueType: none; Flags: deletekey; Tasks: not contextmenu
Root: HKCU; Subkey: "Software\Classes\Directory\shell\Solo"; ValueType: none; Flags: deletekey; Tasks: not contextmenu
Root: HKCU; Subkey: "Software\Classes\Directory\Background\shell\Solo"; ValueType: none; Flags: deletekey; Tasks: not contextmenu
Root: HKCU; Subkey: "Software\Classes\Drive\shell\Solo"; ValueType: none; Flags: deletekey; Tasks: not contextmenu

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
