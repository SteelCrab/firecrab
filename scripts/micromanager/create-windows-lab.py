#!/usr/bin/env python3
"""Create a fresh Windows 11 evaluation VM for issue #266 (Linux/KVM lab)."""
import argparse
import hashlib
import os
from pathlib import Path
import secrets
import shutil
import subprocess
import xml.etree.ElementTree as ET


def run(*args):
    result = subprocess.run(args, text=True, capture_output=True)
    if result.returncode:
        raise SystemExit(f'{args[0]} failed: {result.stderr.strip()}')
    return result.stdout


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--iso', type=Path, required=True)
    parser.add_argument('--state', type=Path, default=Path.home() / '.local/share/firecrab/windows-lab')
    args = parser.parse_args()
    iso = args.iso.resolve()
    state = args.state.resolve()
    name = 'firecrab-windows-lab'
    uri = 'qemu:///session'
    for tool in ('virsh', 'virt-install', 'qemu-img', 'swtpm', 'xorriso'):
        if not shutil.which(tool):
            parser.error(f'Missing executable: {tool}')
    if not os.access('/dev/kvm', os.R_OK | os.W_OK):
        parser.error('Current user needs read/write access to /dev/kvm')
    if name in run('virsh', '-c', uri, 'list', '--all', '--name').splitlines():
        parser.error(f'{name} already exists; refusing to replace it')
    if (state / 'windows.qcow2').exists():
        parser.error('Disk already exists; refusing to overwrite it')
    expected = 'a61adeab895ef5a4db436e0a7011c92a2ff17bb0357f58b13bbc4062e535e7b9'
    print('Verifying Microsoft Windows 11 Enterprise 25H2 en-US evaluation ISO...', flush=True)
    with iso.open('rb') as f:
        actual = hashlib.file_digest(f, 'sha256').hexdigest()
    if actual != expected:
        parser.error(f'ISO SHA256 mismatch: {actual}')
    os.umask(0o077)
    state.mkdir(parents=True, exist_ok=True)
    state.chmod(0o700)
    seed = state / 'seed'
    seed.mkdir(exist_ok=True)
    password = secrets.token_hex(14) + 'aA1!'
    (state / 'credentials.txt').write_text(f'Username: developer\nPassword: {password}\n')
    # Setup only sees a newly created, empty virtual disk. No host disks are attached.
    (seed / 'Autounattend.xml').write_text(fr'''<?xml version="1.0" encoding="utf-8"?>
<unattend xmlns="urn:schemas-microsoft-com:unattend" xmlns:wcm="http://schemas.microsoft.com/WMIConfig/2002/State">
 <settings pass="windowsPE">
  <component name="Microsoft-Windows-International-Core-WinPE" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
   <SetupUILanguage><UILanguage>en-US</UILanguage></SetupUILanguage><InputLocale>0409:00000409</InputLocale><SystemLocale>en-US</SystemLocale><UILanguage>en-US</UILanguage><UserLocale>en-US</UserLocale>
  </component>
  <component name="Microsoft-Windows-Setup" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
   <DiskConfiguration><Disk wcm:action="add"><DiskID>0</DiskID><WillWipeDisk>true</WillWipeDisk>
    <CreatePartitions>
     <CreatePartition wcm:action="add"><Order>1</Order><Type>EFI</Type><Size>260</Size></CreatePartition>
     <CreatePartition wcm:action="add"><Order>2</Order><Type>MSR</Type><Size>16</Size></CreatePartition>
     <CreatePartition wcm:action="add"><Order>3</Order><Type>Primary</Type><Extend>true</Extend></CreatePartition>
    </CreatePartitions>
    <ModifyPartitions>
     <ModifyPartition wcm:action="add"><Order>1</Order><PartitionID>1</PartitionID><Format>FAT32</Format><Label>System</Label></ModifyPartition>
     <ModifyPartition wcm:action="add"><Order>2</Order><PartitionID>3</PartitionID><Format>NTFS</Format><Label>Windows</Label><Letter>C</Letter></ModifyPartition>
    </ModifyPartitions>
   </Disk><WillShowUI>OnError</WillShowUI></DiskConfiguration>
   <ImageInstall><OSImage><InstallFrom><MetaData wcm:action="add"><Key>/IMAGE/INDEX</Key><Value>1</Value></MetaData></InstallFrom><InstallTo><DiskID>0</DiskID><PartitionID>3</PartitionID></InstallTo><WillShowUI>OnError</WillShowUI></OSImage></ImageInstall>
   <UserData><AcceptEula>true</AcceptEula><FullName>Firecrab Developer</FullName><Organization>Firecrab Lab</Organization></UserData>
  </component>
 </settings>
 <settings pass="specialize">
  <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS"><ComputerName>FIRECRAB-WIN</ComputerName><TimeZone>Korea Standard Time</TimeZone></component>
 </settings>
 <settings pass="oobeSystem">
  <component name="Microsoft-Windows-International-Core" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS"><InputLocale>0409:00000409</InputLocale><SystemLocale>en-US</SystemLocale><UILanguage>en-US</UILanguage><UserLocale>en-US</UserLocale></component>
  <component name="Microsoft-Windows-Shell-Setup" processorArchitecture="amd64" publicKeyToken="31bf3856ad364e35" language="neutral" versionScope="nonSxS">
   <OOBE><HideEULAPage>true</HideEULAPage><HideOnlineAccountScreens>true</HideOnlineAccountScreens><HideWirelessSetupInOOBE>true</HideWirelessSetupInOOBE><ProtectYourPC>3</ProtectYourPC></OOBE>
   <UserAccounts><LocalAccounts><LocalAccount wcm:action="add"><Name>developer</Name><DisplayName>Firecrab Developer</DisplayName><Group>Administrators</Group><Password><Value>{password}</Value><PlainText>true</PlainText></Password></LocalAccount></LocalAccounts></UserAccounts>
   <AutoLogon><Password><Value>{password}</Value><PlainText>true</PlainText></Password><Username>developer</Username><Enabled>true</Enabled><LogonCount>1</LogonCount></AutoLogon>
   <FirstLogonCommands><SynchronousCommand wcm:action="add"><Order>1</Order><Description>Record lab baseline</Description><CommandLine>powershell.exe -NoProfile -ExecutionPolicy Bypass -Command "Get-Volume | Where-Object FileSystemLabel -eq 'FIRECRAB' | ForEach-Object {{ &amp; ($_.DriveLetter + ':\baseline.ps1') }}"</CommandLine></SynchronousCommand></FirstLogonCommands>
  </component>
 </settings>
</unattend>''')
    (seed / 'baseline.ps1').write_text(r'''$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Force C:\firecrab-lab | Out-Null
$report = [ordered]@{
    OS = Get-CimInstance Win32_OperatingSystem | Select-Object Caption, Version, BuildNumber, OSArchitecture
    CPU = Get-CimInstance Win32_Processor | Select-Object Name, NumberOfCores, NumberOfLogicalProcessors, VirtualizationFirmwareEnabled, SecondLevelAddressTranslationExtensions, VMMonitorModeExtensions
    Computer = Get-CimInstance Win32_ComputerSystem | Select-Object TotalPhysicalMemory, HypervisorPresent
    Network = Get-NetIPConfiguration | Out-String
    TPM = Get-Tpm | Select-Object TpmPresent, TpmReady
}
$json = $report | ConvertTo-Json -Depth 5
$json | Set-Content C:\firecrab-lab\baseline.json
try {
    $port = New-Object System.IO.Ports.SerialPort COM1,115200,None,8,one
    $port.Open(); $port.WriteLine('FIRECRAB_BASELINE_BEGIN'); $port.WriteLine($json); $port.WriteLine('FIRECRAB_BASELINE_END'); $port.Close()
} catch { $_ | Out-File C:\firecrab-lab\serial-error.txt }
# Remove installation auto-logon credentials after the first login.
$winlogon = 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Winlogon'
Set-ItemProperty $winlogon AutoAdminLogon '0'
Remove-ItemProperty $winlogon DefaultPassword -ErrorAction SilentlyContinue
''')
    run('xorriso', '-as', 'mkisofs', '-quiet', '-J', '-R', '-V', 'FIRECRAB', '-o', str(state / 'seed.iso'), str(seed))
    run('qemu-img', 'create', '-f', 'qcow2', str(state / 'windows.qcow2'), '96G')
    xml = run('virt-install', '--connect', uri, '--name', name, '--memory', '4096', '--vcpus', '4',
              '--cpu', 'host-passthrough,+vmx', '--machine', 'q35', '--boot', 'uefi',
              '--tpm', 'backend.type=emulator,backend.version=2.0,model=tpm-crb',
              '--disk', f'{state}/windows.qcow2,bus=sata,format=qcow2', '--cdrom', str(iso),
              '--disk', f'{state}/seed.iso,device=cdrom,bus=sata,readonly=on',
              '--network', 'user,model=e1000e', '--graphics', 'vnc,listen=127.0.0.1',
              '--video', 'vga', '--serial', f'file,path={state}/serial.log',
              '--osinfo', 'win11', '--noautoconsole', '--print-xml=1')
    domain = ET.fromstring(xml)
    domain.find('on_reboot').text = 'restart'
    firmware = ET.SubElement(domain.find('os'), 'firmware')
    ET.SubElement(firmware, 'feature', enabled='yes', name='secure-boot')
    ET.SubElement(firmware, 'feature', enabled='yes', name='enrolled-keys')
    domain.find('cpu/feature[@name="vmx"]').set('policy', 'require')
    # win11 osinfo defaults to hv-evmcs, which hangs Windows 11's VBS hypervisor
    # at a black screen right after Boot Manager on nested-KVM.
    domain.find('features/hyperv/evmcs').set('state', 'off')
    config = state / 'domain.xml'
    ET.ElementTree(domain).write(config, encoding='unicode')
    run('virsh', '-c', uri, 'define', str(config))
    run('virsh', '-c', uri, 'start', name)
    print(f'VM started. Credentials: {state}/credentials.txt')
    print(f'Console: virt-manager --connect {uri} --show-domain-console {name}')
    print('Press any key in the console if the Windows installation DVD prompts.')


if __name__ == '__main__':
    main()
