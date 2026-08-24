#!/usr/bin/env python3
from pathlib import Path

path = Path("scripts/firecracker-menual/install-alpine-rootfs.sh")
text = path.read_text(encoding="utf-8")
if "spdx-licenses-text" in text:
    raise SystemExit(0)
old = "# bash: Shell repository scripts often use #!/bin/bash (same as Ubuntu).\nrootfs_packages='alpine-baselayout busybox bash openrc agetty iproute2-minimal iputils-ping dhcpcd openssh-server ca-certificates curl procps linux-virt'\n"
new = "# bash: Shell repository scripts often use #!/bin/bash (same as Ubuntu).\n# spdx-licenses-text: Alpine's normal runtime packages intentionally omit most\n# license files. Keep the distribution-packaged SPDX License List text corpus\n# in the image so release packaging can recover a complete canonical license\n# bundle without fetching mutable web content or inventing license text.\nrootfs_packages='alpine-baselayout busybox bash openrc agetty iproute2-minimal iputils-ping dhcpcd openssh-server ca-certificates curl procps linux-virt spdx-licenses-text'\n"
if text.count(old) != 1:
    raise SystemExit(f"Alpine package anchor mismatch: {text.count(old)}")
path.write_text(text.replace(old, new, 1), encoding="utf-8")
