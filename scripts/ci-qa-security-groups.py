#!/usr/bin/env python3
"""Exercise production nft rules with real TAP frames in a disposable netns.

Export fixtures with the helper's export_security_group_packet_fixtures test.
Run this script ONLY in a fresh network namespace (or --network none container):
  python3 scripts/ci-qa-security-groups.py FIXTURES --br-netfilter 0
  python3 scripts/ci-qa-security-groups.py FIXTURES --br-netfilter 1
No network operation is performed until the fresh-namespace guard passes.
"""

import argparse
import fcntl
import ipaddress
import json
import os
from pathlib import Path
import select
import socket
import struct
import subprocess
import time


def run(*args, **kwargs):
    result = subprocess.run(args, capture_output=True, **kwargs)
    if result.returncode:
        raise RuntimeError(f"{args}: {result.stderr.decode(errors='replace')}")
    return result


def checksum(data):
    if len(data) % 2:
        data += b"\0"
    total = sum(struct.unpack(f"!{len(data) // 2}H", data))
    while total >> 16:
        total = (total & 65535) + (total >> 16)
    return (~total) & 65535


def mac(value):
    return bytes.fromhex(value.replace(":", ""))


def ip(value):
    return ipaddress.IPv4Address(value).packed


def packet(source, destination, sport, dport, protocol=6, flags=2, seq=100, ack=0):
    if protocol == 6:
        payload = struct.pack("!HHIIBBHHH", sport, dport, seq, ack, 80, flags, 65535, 0, 0)
        pseudo = ip(source) + ip(destination) + struct.pack("!BBH", 0, 6, len(payload))
        payload = payload[:16] + struct.pack("!H", checksum(pseudo + payload)) + payload[18:]
    else:
        payload = struct.pack("!HHHH", sport, dport, 12, 0) + b"sg59"
    header = struct.pack("!BBHHHBBH4s4s", 69, 0, 20 + len(payload), 0, 0, 64, protocol, 0, ip(source), ip(destination))
    return header[:10] + struct.pack("!H", checksum(header)) + header[12:] + payload


class Tap:
    def __init__(self, name, address, hardware, bridge=None):
        self.ip, self.mac = address, hardware
        self.fd = os.open("/dev/net/tun", os.O_RDWR | os.O_NONBLOCK)
        fcntl.ioctl(self.fd, 0x400454CA, struct.pack("16sH", name.encode(), 0x1002))
        run("ip", "link", "set", name, "up")
        if bridge:
            run("ip", "link", "set", name, "master", bridge)
            run("bridge", "fdb", "replace", hardware, "dev", name, "master", "static")

    def send(self, destination, sport, dport, destination_mac, **kwargs):
        source = kwargs.pop("source", self.ip)
        frame = mac(destination_mac) + mac(self.mac) + b"\x08\x00" + packet(source, destination, sport, dport, **kwargs)
        os.write(self.fd, frame)

    def receive(self, sport, dport, timeout=0.25):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            if not select.select([self.fd], [], [], max(0, deadline - time.monotonic()))[0]:
                break
            data = os.read(self.fd, 65536)
            if len(data) < 38 or data[12:14] != b"\x08\x00" or data[23] not in (6, 17):
                continue
            offset = 14 + (data[14] & 15) * 4
            if struct.unpack("!HH", data[offset:offset + 4]) == (sport, dport):
                return data[14:]
        return None


def expect(label, result):
    if not result:
        raise AssertionError(label)
    print(f"PASS {label}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fixtures", type=Path)
    parser.add_argument("--br-netfilter", type=int, choices=(0, 1), required=True)
    parser.add_argument("--helper-tests", type=Path, help="helper test binary for real conntrack cleanup")
    args = parser.parse_args()
    # A netns has no routed Ethernet interfaces on entry. Refuse an actual host.
    links = json.loads(run("ip", "-d", "-j", "link").stdout)
    tunnel_kinds = {"ipip", "gre", "gretap", "erspan", "vti", "vti6", "sit", "ip6tnl", "ip6gre"}
    if any(link["ifname"] != "lo" and (link.get("linkinfo", {}).get("info_kind") not in tunnel_kinds or "UP" in link["flags"]) for link in links):
        raise SystemExit("refusing network changes: use a fresh network namespace")
    if os.geteuid() != 0:
        raise SystemExit("root is required inside the isolated namespace")
    run("ip", "link", "set", "lo", "up")
    forwarding = Path("/proc/sys/net/ipv4/ip_forward")
    if forwarding.read_text().strip() != "1":
        forwarding.write_text("1")
    topology = json.loads((args.fixtures / "topology.json").read_text())
    bridges = topology["bridges"]
    for bridge in bridges:
        run("ip", "link", "add", bridge["name"], "type", "bridge", "stp_state", "0", "forward_delay", "0")
        run("ip", "address", "add", bridge["gateway"] + "/24", "dev", bridge["name"])
        run("ip", "link", "set", bridge["name"], "up")
        bridge["mac"] = Path(f"/sys/class/net/{bridge['name']}/address").read_text().strip()
    switch = Path("/proc/sys/net/bridge/bridge-nf-call-iptables")
    if not switch.exists():
        raise SystemExit("br_netfilter must be available before entering the namespace")
    if switch.read_text().strip() != str(args.br_netfilter):
        switch.write_text(str(args.br_netfilter))
    taps = [Tap(vm["tap"], vm["ip"], vm["mac"], vm["bridge"]) for vm in topology["vms"]]
    a, b, c, d = taps
    for vm in topology["vms"]:
        run("ip", "neigh", "replace", vm["ip"], "lladdr", vm["mac"], "nud", "permanent", "dev", vm["bridge"])
    wan = Tap("sgwan", "192.0.2.2", "02:00:00:00:01:02")
    run("ip", "address", "add", "192.0.2.1/24", "dev", "sgwan")
    run("ip", "neigh", "replace", wan.ip, "lladdr", wan.mac, "nud", "permanent", "dev", "sgwan")
    wan_mac = Path("/sys/class/net/sgwan/address").read_text().strip()
    gateway = bridges[0]["gateway"]
    gateway_mac = Path(f"/sys/class/net/{bridges[0]['name']}/address").read_text().strip()

    def apply(mode):
        run("nft", "-f", str(args.fixtures / f"{mode}.nft"))
        run("conntrack", "-F")

    apply("normal")
    a.send(b.ip, 40001, 5432, b.mac)
    expect("allowed TCP ingress and egress", b.receive(40001, 5432))
    b.send(a.ip, 5432, 40001, a.mac, flags=18, seq=200, ack=101)
    expect("stateful TCP reply with no reverse allow rule", a.receive(5432, 40001))
    a.send(b.ip, 40001, 5432, b.mac, flags=16, seq=101, ack=201)
    b.receive(40001, 5432)
    a.send(b.ip, 40002, 5433, b.mac)
    expect("destination port range", b.receive(40002, 5433))
    a.send(b.ip, 40003, 5434, b.mac)
    expect("unlisted TCP port denied", b.receive(40003, 5434) is None)
    c.send(b.ip, 40004, 5432, b.mac)
    expect("ingress denied for unlisted peer", b.receive(40004, 5432) is None)
    c.send(b.ip, 40024, 5432, gateway_mac)
    expect("routing through gateway cannot bypass ingress", b.receive(40024, 5432) is None)
    a.send(c.ip, 40025, 5432, gateway_mac)
    expect("routing through gateway cannot bypass egress", c.receive(40025, 5432) is None)
    a.send(b.ip, 40026, 5432, gateway_mac)
    expect("allowed same-network routing through gateway", b.receive(40026, 5432))
    a.send(c.ip, 40005, 5432, c.mac)
    expect("egress denied to ungrouped peer", c.receive(40005, 5432) is None)
    c.send(b.ip, 40027, 5432, b.mac, source=a.ip)
    expect("spoofed allowed peer denied by source pin", b.receive(40027, 5432) is None)
    a.send(b.ip, 40006, 5353, b.mac, protocol=17)
    expect("allowed UDP", b.receive(40006, 5353))
    b.send(a.ip, 5353, 40006, a.mac, protocol=17)
    expect("stateful UDP reply", a.receive(5353, 40006))
    if args.helper_tests:
        environment = dict(os.environ, FIRECRAB_SG_QA_NETNS=os.readlink("/proc/self/ns/net"))
        run(str(args.helper_tests), "security_groups::tests::clear_packet_qa_connections", "--exact", "--ignored", env=environment)
        b.send(a.ip, 5353, 40006, a.mac, protocol=17)
        expect("lease cleanup revokes stale tracked permissions", a.receive(5353, 40006) is None)
    b.send(a.ip, 40007, 5432, a.mac)
    expect("reverse new connection denied", a.receive(40007, 5432) is None)
    # An ungrouped VM retains its same-network connectivity.
    c.send(a.ip, 40008, 9999, a.mac)
    expect("empty ingress denies new connections", a.receive(40008, 9999) is None)
    a.send(d.ip, 40009, 443, gateway_mac)
    expect("security group cannot bypass cross-network isolation", d.receive(40009, 443) is None)
    a.send(wan.ip, 40010, 443, gateway_mac)
    outgoing = wan.receive(40010, 443)
    expect("allowed routed Internet egress", outgoing)
    c.send(wan.ip, 40028, 443, gateway_mac)
    expect("ungrouped VM retains legacy egress", wan.receive(40028, 443))
    wan.send("192.0.2.1", 443, 40010, wan_mac, flags=18, seq=200, ack=101)
    expect("routed established reply", a.receive(443, 40010))
    wan.send("192.0.2.1", 40011, 18080, wan_mac)
    expect("DNAT cannot bypass ingress", b.receive(40011, 5432) is None)

    for port, source, destination, label in [(53, a.ip, gateway, "gateway DNS"), (67, "0.0.0.0", "255.255.255.255", "DHCP broadcast"), (12345, a.ip, gateway, "other new VM-to-host traffic denied")]:
        receiver = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        receiver.settimeout(0.3)
        receiver.bind(("0.0.0.0", port))
        a.send(destination, 68 if port == 67 else 40012, port, "ff:ff:ff:ff:ff:ff" if port == 67 else gateway_mac, source=source, protocol=17)
        try:
            received = receiver.recv(1024) == b"sg59"
        except socket.timeout:
            received = False
        receiver.close()
        expect(label, received if port != 12345 else not received)

    admin = socket.socket()
    admin.setblocking(False)
    admin.connect_ex((a.ip, 22))
    admin_port = admin.getsockname()[1]
    syn = a.receive(admin_port, 22)
    expect("host-originated administration", syn)
    seq = struct.unpack("!I", syn[24:28])[0]
    a.send(gateway, 22, admin_port, gateway_mac, flags=18, seq=700, ack=seq + 1)
    expect("administration reply through host input", a.receive(admin_port, 22))
    admin.close()

    apply("external")
    wan.send("192.0.2.1", 40013, 18080, wan_mac)
    expect("DNAT works with explicit ingress allow", b.receive(40013, 5432))
    for mode in ("isolated", "offline", "empty"):
        apply(mode)
        a.send(wan.ip, 40014, 443, gateway_mac)
        expect(f"{mode} denies Internet despite SG rule", wan.receive(40014, 443) is None)
    apply("normal")
    before = run("nft", "list", "ruleset").stdout
    failed = subprocess.run(["nft", "-f", "-"], input=(args.fixtures / "empty.nft").read_bytes() + b"add rule inet firecrab_sg missing_chain drop\n", capture_output=True)
    expect("invalid replacement rejected atomically", failed.returncode != 0 and before == run("nft", "list", "ruleset").stdout)
    a.send(b.ip, 40015, 5432, b.mac)
    expect("previous policy remains after failed replacement", b.receive(40015, 5432))
    print(f"PASS TAP/bridge packet QA, br_netfilter={args.br_netfilter}", flush=True)


if __name__ == "__main__":
    main()
