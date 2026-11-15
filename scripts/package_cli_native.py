"""Build native CLI installers from an already validated private payload."""
from pathlib import Path
import os
import platform
import re
import shlex
import shutil
import subprocess
import tempfile
import time

import package_metadata

from native_targets import INSTALLER_FORMATS, NATIVE_PLATFORMS

FORMATS = dict(INSTALLER_FORMATS)
REGISTRY_KEY = r'Software\Stream Dock Manager\stream-dock-manager-cli'
UNINSTALL_KEY = r'Software\Microsoft\Windows\CurrentVersion\Uninstall\stream-dock-manager-cli'


# Run the selected command and propagate failure to the calling workflow
def run(args, *, cwd=None, capture=False):
    result = subprocess.run([str(arg) for arg in args], cwd=cwd, check=True,
                            stdout=subprocess.PIPE if capture else None, text=capture)
    return result.stdout if capture else None


# Resolve the required native installer tool
def tool(variable, fallback):
    selected = os.environ.get(variable)
    if selected:
        path = Path(selected)
        if not path.is_absolute() or not path.is_file():
            raise ValueError(f'{variable} must select an absolute installed tool executable')
        return str(path)
    found = shutil.which(fallback)
    if not found:
        raise ValueError(f'Missing native packaging tool: {fallback}')
    return found


# Escape a value for inclusion in the Windows installer script
def nsis_string(value):
    value = str(value)
    if any(c in value for c in '\r\n\0'):
        raise ValueError('Invalid installer string')
    return '"' + value.replace('$', '$$').replace('"', '$\\"') + '"'


# Render the Windows CLI installer recipe from validated metadata
def nsis_recipe(payload, output, version):
    metadata = package_metadata.load()
    publisher = nsis_string(metadata['package']['publisher'])
    website = nsis_string(metadata['package']['website'])
    copyright_text = nsis_string(metadata['package']['copyright'])
    description = nsis_string(metadata['cli']['summary'])
    files = sorted(path.name for path in payload.iterdir())
    install = '\n'.join(f'  File {nsis_string(payload / name)}' for name in files)
    remove = '\n'.join(f'  Delete "$INSTDIR\\{name}"' for name in files)
    return f'''Unicode true
RequestExecutionLevel user
Name "Stream Dock Manager CLI"
OutFile {nsis_string(output)}
InstallDir "$LOCALAPPDATA\\Programs\\stream-dock-manager-cli"
SetCompressor /SOLID lzma
VIProductVersion "{version}.0"
VIAddVersionKey /LANG=1033 "ProductName" "Stream Dock Manager CLI"
VIAddVersionKey /LANG=1033 "CompanyName" {publisher}
VIAddVersionKey /LANG=1033 "LegalCopyright" {copyright_text}
VIAddVersionKey /LANG=1033 "FileDescription" {description}
VIAddVersionKey /LANG=1033 "FileVersion" "{version}"
!include "MUI2.nsh"
!include "x64.nsh"
!insertmacro MUI_PAGE_LICENSE {nsis_string(payload / 'LICENSE')}
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
Function .onInit
  SetRegView 64
  SetShellVarContext current
  ${{IfNot}} ${{RunningX64}}
    MessageBox MB_OK "Stream Dock Manager CLI requires Windows x64."
    Abort
  ${{EndIf}}
  ReadRegStr $0 HKCU "{REGISTRY_KEY}" "InstallerKind"
  StrCmp $0 "msi" 0 +3
    MessageBox MB_OK "Uninstall the MSI edition of Stream Dock Manager CLI before switching installer formats. Your Manager data is retained." /SD IDOK
    Abort
FunctionEnd
Function un.onInit
  SetRegView 64
  SetShellVarContext current
FunctionEnd
Section "Stream Dock Manager CLI"
  SetOutPath "$INSTDIR"
{install}
  WriteUninstaller "$INSTDIR\\Uninstall.exe"
  WriteRegStr HKCU "{REGISTRY_KEY}" "InstallerKind" "nsis"
  WriteRegStr HKCU "{UNINSTALL_KEY}" "DisplayName" "Stream Dock Manager CLI"
  WriteRegStr HKCU "{UNINSTALL_KEY}" "DisplayVersion" "{version}"
  WriteRegStr HKCU "{UNINSTALL_KEY}" "Publisher" {publisher}
  WriteRegStr HKCU "{UNINSTALL_KEY}" "URLInfoAbout" {website}
  WriteRegStr HKCU "{UNINSTALL_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "{UNINSTALL_KEY}" "UninstallString" '$\\"$INSTDIR\\Uninstall.exe$\\"'
  WriteRegDWORD HKCU "{UNINSTALL_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "{UNINSTALL_KEY}" "NoRepair" 1
SectionEnd
Section "Uninstall"
{remove}
  Delete "$INSTDIR\\Uninstall.exe"
  RMDir "$INSTDIR"
  DeleteRegValue HKCU "{REGISTRY_KEY}" "InstallerKind"
  DeleteRegKey /ifempty HKCU "{REGISTRY_KEY}"
  DeleteRegKey HKCU "{UNINSTALL_KEY}"
SectionEnd
'''


# Build the Windows CLI installer from the staged payload
def write_windows(payload, work, output, stem, version):
    windows_host = os.name == 'nt'
    makensis = tool('STREAM_DOCK_MANAGER_NSIS_MAKENSIS', 'makensis.exe' if windows_host else 'makensis')
    exe = output / f'{stem}.exe'
    script = work / 'installer.nsi'
    script.write_text(nsis_recipe(payload, exe, version), encoding='utf-8')
    option = '/' if windows_host else '-'
    run([makensis, option + 'NOCONFIG', option + 'V2', script])
    return [exe]


RELOAD_RULES = 'if command -v udevadm >/dev/null 2>&1; then udevadm control --reload-rules >/dev/null 2>&1 || :; fi\n'


# Stage CLI binaries and metadata for Linux installers
def linux_payload(payload, root, rules):
    files = {}
    for source in payload.iterdir():
        relative = Path('usr/bin/stream-dock-manager-cli') if source.name == 'stream-dock-manager-cli' else Path('usr/share/doc/stream-dock-manager-cli') / source.name
        target = root / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        target.chmod(0o755 if source.name == 'stream-dock-manager-cli' else 0o644)
        files['/' + relative.as_posix()] = target
    target = root / 'usr/lib/udev/rules.d/60-stream-dock-manager-cli.rules'
    target.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(rules, target)
    target.chmod(0o644)
    files['/usr/lib/udev/rules.d/60-stream-dock-manager-cli.rules'] = target
    # mkdir's mode is filtered by the builder's umask. Package directories must
    # remain traversable after installation regardless of that private setting
    for directory in (root, *[path for path in root.rglob('*') if path.is_dir()]):
        directory.chmod(0o755)
    return files


# Render the RPM package specification from validated metadata
def rpm_recipe(payload, files, version, metadata=None):
    metadata = metadata or package_metadata.load()
    common, cli, linux = metadata['package'], metadata['cli'], metadata['linux']
    # RPM spec fields expand macros; literal user values must not execute them
    escape = lambda value: value.replace('%', '%%')
    quoted = shlex.quote(str(payload)).replace('%', '%%')
    owned = '\n'.join('"' + path + '"' for path in sorted(files))
    return f'''Name: stream-dock-manager-cli
Version: {version}
Release: {linux['rpm_release']}
Summary: {escape(cli['summary'])}
License: {escape(cli['license'])}
URL: {escape(common['website'])}
Vendor: {escape(common['publisher'])}
Packager: {escape(common['publisher'])}
BuildArch: x86_64
Requires: systemd-udev
Recommends: ffmpeg
%global debug_package %{{nil}}
%global __os_install_post %{{nil}}
%description
{escape(cli['description'])}
%install
mkdir -p "%{{buildroot}}"
cp -a {quoted}/. "%{{buildroot}}/"
%post
{RELOAD_RULES}%postun
{RELOAD_RULES}%files
%dir /usr/share/doc/stream-dock-manager-cli
{owned}
'''


# Build the Linux CLI installers from the staged payload
def write_linux(payload, work, output, stem, version, rules):
    deb_tool = tool('STREAM_DOCK_MANAGER_DPKG_DEB', 'dpkg-deb')
    shlib_tool = tool('STREAM_DOCK_MANAGER_DPKG_SHLIBDEPS', 'dpkg-shlibdeps')
    rpm_tool = tool('STREAM_DOCK_MANAGER_RPMBUILD', 'rpmbuild')
    metadata = package_metadata.load()
    common, cli, linux = metadata['package'], metadata['cli'], metadata['linux']
    root = work / 'linux-root'
    files = linux_payload(payload, root, rules)
    debian = work / 'debian'
    debian.mkdir()
    (debian / 'control').write_text(f"Source: stream-dock-manager-cli\nSection: {linux['section']}\nPriority: {linux['priority']}\nMaintainer: {common['publisher']}\n\nPackage: stream-dock-manager-cli\nArchitecture: amd64\nDescription: {cli['summary']}\n", encoding='utf-8')
    # Let dpkg-shlibdeps identify the staged package root before scanning the binary
    control = root / 'DEBIAN'
    control.mkdir()
    control.chmod(0o755)
    dependencies = run([shlib_tool, '-O', '-e', files['/usr/bin/stream-dock-manager-cli']], cwd=work, capture=True)
    dependencies = [line.removeprefix('shlibs:Depends=') for line in dependencies.splitlines() if line.startswith('shlibs:Depends=')]
    if len(dependencies) != 1:
        raise ValueError('Could not determine native CLI library dependencies')
    depends = ', '.join(filter(None, (dependencies[0].strip(), 'udev')))
    (control / 'control').write_text(f'Package: stream-dock-manager-cli\nVersion: {version}\nArchitecture: amd64\nMaintainer: {common["publisher"]}\nHomepage: {common["website"]}\nSection: {linux["section"]}\nPriority: {linux["priority"]}\nDepends: {depends}\nRecommends: ffmpeg\nDescription: {cli["summary"]}\n {cli["description"]}\n', encoding='utf-8')
    for script in ('postinst', 'postrm'):
        path = control / script
        path.write_text('#!/bin/sh\nset -e\n' + RELOAD_RULES, encoding='utf-8')
        path.chmod(0o755)
    deb = output / f'{stem}.deb'
    run([deb_tool, '--root-owner-group', '--build', root, deb])
    # RPM owns the same payload, without Debian's package-control directory
    shutil.rmtree(control)
    top = work / 'rpmbuild'
    for folder in ('BUILD', 'BUILDROOT', 'RPMS', 'SOURCES', 'SPECS', 'SRPMS'):
        (top / folder).mkdir(parents=True, exist_ok=True)
    spec = top / 'SPECS/stream-dock-manager-cli.spec'
    spec.write_text(rpm_recipe(root, files, version, metadata), encoding='utf-8')
    run([rpm_tool, '-bb', '--define', f'_topdir {top}', '--define', '_binary_filedigest_algorithm 8', spec])
    rpms = list((top / 'RPMS').rglob('*.rpm'))
    if len(rpms) != 1:
        raise ValueError('Expected exactly one native CLI RPM')
    rpm = output / f'{stem}.rpm'
    shutil.move(rpms[0], rpm)
    return [deb, rpm]


# Retry transient disk-image service contention without reusing partially created images
def create_macos_image(payload, image, version):
    if image.exists():
        raise ValueError('Refusing to overwrite an existing CLI disk image')
    for attempt in range(3):
        with tempfile.TemporaryDirectory(prefix='.dmg-attempt-', dir=image.parent) as temporary:
            candidate = Path(temporary) / image.name
            command = ['/usr/bin/hdiutil', 'create', '-volname', f'Stream Dock Manager CLI {version}',
                       '-srcfolder', str(payload), '-format', 'UDZO', str(candidate)]
            result = subprocess.run(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
            if result.stdout:
                print(result.stdout, end='', flush=True)
            if result.returncode == 0:
                run(['/usr/bin/hdiutil', 'verify', candidate])
                candidate.rename(image)
                return
            if attempt == 2 or 'hdiutil: create failed - Resource busy' not in result.stdout:
                raise subprocess.CalledProcessError(result.returncode, command, output=result.stdout)
        time.sleep(5 * (attempt + 1))



# Dispatch native installer generation for the selected platform
def build_native(payload, work, output, osname, arch, version, rules=None):
    hosts = {'macos': {'Darwin'}, 'windows': {'Windows', 'Darwin', 'Linux'}, 'linux': {'Linux'}}
    if platform.system() not in hosts.get(osname, set()):
        raise ValueError(f'CLI packaging for {osname} is unavailable on this host')
    if (osname, arch) not in NATIVE_PLATFORMS:
        raise ValueError('Unsupported native CLI architecture')
    if not re.fullmatch(r'(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)', version):
        raise ValueError('Invalid native CLI version')
    stem = f'stream-dock-manager-cli-{version}-{osname}-{arch}'
    if osname == 'macos':
        image = output / f'{stem}.dmg'
        create_macos_image(payload, image, version)
        paths = [image]
    elif osname == 'windows':
        paths = write_windows(payload, work, output, stem, version)
    else:
        if rules is None or not rules.is_file() or rules.is_symlink():
            raise ValueError('Generated CLI device rules are required')
        paths = write_linux(payload, work, output, stem, version, rules)
    if {path.suffix for path in paths} != set(FORMATS[osname]) or any(not path.is_file() or path.is_symlink() or path.stat().st_size == 0 for path in paths):
        raise ValueError('Native CLI installer inventory is incomplete')
    return paths
