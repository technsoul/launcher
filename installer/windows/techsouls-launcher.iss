; Inno Setup script for the Windows installer.
; Built in CI with: iscc /DAppVersion=<version> /DSourceExe=<path to exe> techsouls-launcher.iss

#ifndef AppVersion
  #define AppVersion "0.0.0"
#endif
#ifndef SourceExe
  #define SourceExe "..\..\target\release\techsouls-launcher.exe"
#endif

[Setup]
; Never change AppId: Windows uses it to find the existing install on upgrade.
AppId={{6C2B8E0A-4F1D-4B7E-9A53-2E7C1D9F4A10}
AppName=TechSouls Launcher
AppVersion={#AppVersion}
AppVerName=TechSouls Launcher {#AppVersion}
AppPublisher=TechSouls
AppPublisherURL=https://github.com/technsoul/launcher
; Per-user install: no admin prompt, and the launcher can replace its own
; exe when it updates itself.
PrivilegesRequired=lowest
DefaultDirName={localappdata}\Programs\TechSouls Launcher
DefaultGroupName=TechSouls
DisableProgramGroupPage=yes
UninstallDisplayIcon={app}\techsouls-launcher.exe
UninstallDisplayName=TechSouls Launcher
OutputBaseFilename=techsouls-launcher-windows-setup
Compression=lzma2
SolidCompression=yes
WizardStyle=modern
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
CloseApplications=yes

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "techsouls-launcher.exe"; Flags: ignoreversion

[Icons]
Name: "{userprograms}\TechSouls Launcher"; Filename: "{app}\techsouls-launcher.exe"
Name: "{userdesktop}\TechSouls Launcher"; Filename: "{app}\techsouls-launcher.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\techsouls-launcher.exe"; Description: "{cm:LaunchProgram,TechSouls Launcher}"; Flags: nowait postinstall skipifsilent

[UninstallDelete]
; Left behind by the launcher's self-update.
Type: files; Name: "{app}\techsouls-launcher.exe.old"
