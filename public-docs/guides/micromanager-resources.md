# microManager resources

On macOS and Windows, the management VM is also the execution host for all MicroVMs.
Its CPU, RAM, disk, and networking determine the resources available to those workloads.

## CLI

`resorce` is the supported command spelling. `resource` is an alias.
RAM uses MiB; disk capacity uses GiB. CPU is a virtual CPU count.

```sh
firecrab service resorce
firecrab service resorce --json
firecrab service resorce --cpu 4 --ram 8192
firecrab service resorce --apply
```

The first command reports the saved allocation, physical CPU/RAM capacity, free physical disk space, and pending changes.
Saving writes `<managed-home>/resources.json`. Applying validates again, restarts the management VM, and stops its running MicroVMs.
MicroVMs are not automatically restarted. Ordinary reinstall preserves the allocation.
Values above the physical host or platform limits are rejected.
Disks can only grow; virtual capacity does not reserve the same amount of physical disk space.

### macOS

The default allocation is 2 vCPU, 4096 MiB RAM, and a 16 GiB data disk.
CPU and RAM belong to this management VM. The replaceable OS disk is separate.
Disk growth stops the VM, expands the sparse data image, and grows its ext4 filesystem after boot.
The VM's existing disk data and network MAC address are preserved.

```sh
firecrab service resorce --disk 32 --apply
firecrab service resorce --network bridged --interface en0
firecrab service resorce --network nat
```

Bridged mode uses an available physical interface reported by the signed helper.
It is enabled only with an [Apple-approved `com.apple.vm.networking` entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.vm.networking).
The standard ad-hoc-signed helper supports NAT and rejects bridged mode before saving or stopping the VM.
The build does not add this restricted entitlement to the standard helper, because doing so prevents it from launching.
The existing SSH tunnel and workload TCP forwarding continue to use the management VM's address.

### Windows

CPU, RAM, and NAT/mirrored networking are **global WSL2 settings**, shared with other distributions.
They are read from and merged into `%USERPROFILE%\.wslconfig`; unrelated keys, sections, and comments are retained.
The original file is backed up under `runtime/wslconfig.before-resources` before applying.
The initial CPU/RAM limits use WSL defaults when no explicit settings exist.
The imported disk normally has a 1024 GiB virtual capacity.

```powershell
firecrab service resorce --cpu 4 --ram 8192 --network nat
firecrab service resorce --network mirrored
firecrab service resorce --disk 1536 --apply
```

Mirrored networking requires Windows 11 22H2 or later.
Applying refuses to shut down other running WSL distributions. Stop them first.
Disk growth uses `wsl --manage firecrab-debian --resize`; this requires a WSL version that supports that command.
If resizing fails, the previous `.wslconfig` contents are restored.

See Microsoft's [WSL configuration](https://learn.microsoft.com/en-us/windows/wsl/wsl-config) and [disk management](https://learn.microsoft.com/en-us/windows/wsl/disk-space) documentation for the platform behavior.

## Web Host screen

Start the updated native service with `firecrab service start`, then open the dashboard on this host's `http://localhost:5523` or `http://127.0.0.1:5523`.
The **Host → microManager allocation** section shows physical capacity, the current allocation, and editable saved settings.
Select **Save settings**, then **Apply and restart**. The screen displays apply progress and failure details, and reconnects after the VM restarts.
If a restart closes the connection before the apply acknowledgement arrives, the screen reconciles the persisted operation ID rather than submitting the change again.
The native resource controller listens only on `127.0.0.1:5524`; it accepts the dashboard and Vite's port-8080 origins.
The updated guest API and native controller must report the same persistent controller ID.
Resource writes also carry that ID, so an SSH-forwarded remote dashboard cannot change a different local machine.
Remote and HTTPS dashboards show a localhost access hint and do not attempt to control the browser user's machine.
Linux hosts retain their existing host information view.

An apply failure is also recorded in `<managed-home>/runtime/resource-operation.json`.
Background apply output is in `<managed-home>/runtime/resource-apply.log`.
Saving is blocked while an apply is running. A worker that exits without a result becomes a retryable failure after 30 seconds.
