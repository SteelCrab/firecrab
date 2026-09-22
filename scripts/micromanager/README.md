# Windows host lab for microManager (#266)

This Linux/KVM VM provides a Windows host for later Hyper-V versus WSL2
feasibility tests. Creating it does not establish Windows microManager support.
The eventual stack has several virtualization layers: Linux/KVM → Windows →
Debian/Hyper-V or WSL2 → Firecracker workload. Test results on this lab alone
must not be presented as bare-metal Windows compatibility results.

## Create

Requirements: Intel VMX with nested KVM enabled, access to `/dev/kvm`, Python
3.11+, QEMU/KVM, libvirt's user session, virt-install, OVMF, swtpm and xorriso.
The VM uses 4 vCPUs, 4 GiB RAM, a sparse 96 GiB SATA disk, UEFI, TPM 2.0,
host-passthrough CPU with required VMX, and user-mode outbound networking.
Allow more RAM before running nested Debian/Firecracker workloads.

Download **Windows 11 Enterprise 25H2, English (United States), x64 evaluation**
from [Microsoft Evaluation Center](https://www.microsoft.com/en-us/evalcenter/download-windows-11-enterprise).
This is a 90-day evaluation. The script pins Microsoft's published SHA-256:
`a61adeab895ef5a4db436e0a7011c92a2ff17bb0357f58b13bbc4062e535e7b9`.
It does not accept arbitrary installation media.

```sh
python3 scripts/micromanager/create-windows-lab.py --iso /absolute/path/windows11-enterprise-eval.iso
virt-manager --connect qemu:///session --show-domain-console firecrab-windows-lab
```

Press a key at the DVD boot prompt. Setup then installs Windows on the newly
created virtual disk and creates a local `developer` administrator with a random
password. The script refuses to replace an existing VM or disk.

State defaults to `~/.local/share/firecrab/windows-lab`. Passwords and installation
media are outside the repository. Read `credentials.txt` there for login details.
The private seed ISO/XML also contains this password; do not share or commit it.
The first login records OS/build, CPU virtualization flags, memory, networking,
and TPM in `C:\firecrab-lab\baseline.json` and the host's `serial.log`.
Keep the `FIRECRAB` seed CD attached until the `FIRECRAB_BASELINE_END` marker
appears in `serial.log`; automatic login is disabled after that first login.

## Manage

```sh
virsh -c qemu:///session list --all
virsh -c qemu:///session start firecrab-windows-lab
virsh -c qemu:///session shutdown firecrab-windows-lab
virt-manager --connect qemu:///session --show-domain-console firecrab-windows-lab
```

VNC binds only to `127.0.0.1`; no host directories or inbound guest ports are
shared. No system libvirt network or host firewall changes are required.
After the baseline marker appears, eject both SATA CD-ROMs (`sdb` and `sdc`)
and put the hard disk first in boot order. Keep the private credentials file
for future logins.

## WSL2 result, 2026-09-22

Measured inside this lab, which nests one layer deeper than a real Windows
host. These numbers are not bare-metal Windows results.

| Item | Value |
| --- | --- |
| Windows | 11 Enterprise Evaluation, build 10.0.26200, x64 |
| WSL | 2.7.14.0, kernel 6.18.33.2 |
| Distribution | Debian, installed with `wsl --install -d Debian` |
| Firecracker | v1.17.0, static binary staged from the host |
| MicroVM | Alpine 6.18.44-0-virt, initrd only, 1 vCPU, 256 MiB |

- `/dev/kvm` exists in WSL2 Debian and opens once the user joins `kvm`.
- `vmx` reaches WSL2's `/proc/cpuinfo`, so the nested chain stays unbroken.
- Firecracker booted the microVM to a userspace marker and exited 0, twice.
- Boot took 40.1 s and 40.5 s wall, 29.8 s and 31.2 s of guest kernel clock.
  The same payload took 0.8 s and 2.5 s on the Linux host while this lab VM
  was running. Attribute the gap to the lab's extra layer, not to Windows.

The Debian WSL image ships without `curl` and `wget`, and the lab has no
shared host directory. Stage files by serving them on the host loopback and
fetching with `powershell.exe -c "Invoke-WebRequest ..."`, which reaches the
host through the user-mode gateway at `10.0.2.2`, then read them under
`/mnt/c`.

## Next feasibility gate

Run these same checks against Hyper-V and compare the two backends. Then boot
a workload microVM with a real rootfs and networking, and verify DHCP, DNS,
orderly shutdown and startup recovery, as described in
[issue #266](https://github.com/SteelCrab/firecrab/issues/266).
