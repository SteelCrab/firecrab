import fcntl
import json
from pathlib import Path
import signal
import socket
import socketserver
import struct
import subprocess
import sys
import threading
import time

ROOT = Path('/qa')
lock = threading.Lock()
queries = []


def name_wire(name):
    return b''.join(bytes([len(part)]) + part.encode() for part in name.rstrip('.').split('.')) + b'\0'


def upstream_reply(packet, transport):
    offset = 12
    labels = []
    while packet[offset]:
        length = packet[offset]
        labels.append(packet[offset + 1:offset + 1 + length].decode())
        offset += length + 1
    offset += 1
    qtype, qclass = struct.unpack('!HH', packet[offset:offset + 4])
    question = packet[12:offset + 4]
    with lock:
        queries.append({'name': '.'.join(labels), 'type': qtype, 'transport': transport})
        (ROOT / 'upstream-count.json').write_text(json.dumps({'count': len(queries), 'queries': queries}) + '\n')
    if qtype == 1:
        data = socket.inet_aton('198.51.100.44')
    elif qtype == 28:
        data = socket.inet_pton(socket.AF_INET6, '2001:db8::44')
    elif qtype == 16:
        data = b'\x07fixture'
    elif qtype in (2, 5, 12):
        data = name_wire('fixture.external.test')
    elif qtype == 15:
        data = b'\0\x0a' + name_wire('mail.external.test')
    else:
        return packet[:2] + struct.pack('!HHHHH', 0x8180, 1, 0, 0, 0) + question
    answer = b'\xc0\x0c' + struct.pack('!HHIH', qtype, qclass, 3600, len(data)) + data
    return packet[:2] + struct.pack('!HHHHH', 0x8180, 1, 1, 0, 0) + question + answer


class UDP(socketserver.BaseRequestHandler):
    def handle(self):
        packet, sock = self.request
        sock.sendto(upstream_reply(packet, 'udp'), self.client_address)


class TCP(socketserver.StreamRequestHandler):
    def handle(self):
        while prefix := self.rfile.read(2):
            size = struct.unpack('!H', prefix)[0]
            reply = upstream_reply(self.rfile.read(size), 'tcp')
            self.wfile.write(struct.pack('!H', len(reply)) + reply)
            self.wfile.flush()


def interface(ip):
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        for _, name in socket.if_nameindex():
            try:
                result = fcntl.ioctl(sock, 0x8915, struct.pack('256s', name.encode()))
            except OSError:
                continue
            if socket.inet_ntoa(result[20:24]) == ip:
                return name
    raise ValueError(ip)


legacy, private = interface('10.241.41.2'), interface('10.241.42.2')
auth_mode = sys.argv[1] if len(sys.argv) > 1 else 'address'
private_addresses = ['10.241.42.2']
for row in Path('/proc/net/if_inet6').read_text().splitlines():
    fields = row.split()
    if fields[-1] == private:
        private_addresses.append(socket.inet_ntop(socket.AF_INET6, bytes.fromhex(fields[0])))
auth_targets = private if auth_mode == 'interface' else ','.join(private_addresses)
topology = json.loads((ROOT / 'topology.json').read_text())
client = topology['containers']['firecrab-414-dns-client']['networks']
legacy_mac = client['firecrab-414-dns-legacy-20261026']['MacAddress']
private_mac = client['firecrab-414-dns-private-20261026']['MacAddress']
(ROOT / 'dhcp-hosts').write_text(f'{legacy_mac},10.241.41.3,legacy-client\n{private_mac},10.241.42.3,private-client\n')
(ROOT / 'internal-hosts').write_text('10.241.42.22 backend.internal\n')
config = f'''interface={legacy}
interface={private}
bind-dynamic
except-interface=lo
no-resolv
no-hosts
server=127.0.0.9#15353
local=/internal/
addn-hosts=/qa/internal-hosts
auth-server=private.internal,{auth_targets}
auth-zone=internal,10.241.42.0/24,fd12:414:42::/64
host-record=private.internal,10.241.42.2,fd12:414:42::2
dhcp-range=10.241.41.0,static
dhcp-range=10.241.42.0,static
dhcp-hostsfile=/qa/dhcp-hosts
dhcp-leasefile=/qa/dhcp.leases
pid-file=/qa/dnsmasq-fixture.pid
user=root
log-facility=-
log-dhcp
'''
(ROOT / 'dnsmasq.conf').write_text(config)
if auth_mode == 'groups':
    config = f'''interface={legacy}
bind-dynamic
except-interface=lo
no-resolv
no-hosts
server=127.0.0.9#15353
local=/internal/
addn-hosts=/qa/internal-hosts
dhcp-range=10.241.41.0,static
dhcp-hostsfile=/qa/dhcp-hosts
dhcp-leasefile=/qa/dhcp-legacy.leases
pid-file=/qa/dnsmasq-legacy.pid
user=root
log-facility=-
log-dhcp
'''
    (ROOT / 'dnsmasq.conf').write_text(config)
    (ROOT / 'dnsmasq-private.conf').write_text(f'''interface={private}
bind-dynamic
except-interface=lo
no-resolv
no-poll
no-hosts
local=/#/
local=/internal/
domain=internal
addn-hosts=/qa/internal-hosts
host-record=private.internal,10.241.42.2,fd12:414:42::2
dhcp-range=10.241.42.0,static
dhcp-hostsfile=/qa/dhcp-hosts
dhcp-leasefile=/qa/dhcp-private.leases
pid-file=/qa/dnsmasq-private.pid
user=root
log-facility=-
log-dhcp
''')
(ROOT / 'upstream-count.json').write_text('{"count":0,"queries":[]}\n')
servers = [socketserver.ThreadingUDPServer(('127.0.0.9', 15353), UDP),
           socketserver.ThreadingTCPServer(('127.0.0.9', 15353), TCP)]
for server in servers:
    server.daemon_threads = True
    threading.Thread(target=server.serve_forever, daemon=True).start()
command = ['/qa/runtime/lib/ld-linux-aarch64.so.1', '--library-path',
           '/qa/runtime/lib/aarch64-linux-gnu', '/qa/runtime/usr/sbin/dnsmasq',
           '--keep-in-foreground', '--conf-file=/qa/dnsmasq.conf']
log = (ROOT / 'dnsmasq.log').open('ab')
daemon = subprocess.Popen(command, stdout=log, stderr=log)
private_daemon = None
if auth_mode == 'groups':
    private_daemon = subprocess.Popen(command[:-1] + ['--conf-file=/qa/dnsmasq-private.conf'], stdout=log, stderr=log)
time.sleep(.3)
assert daemon.poll() is None, (ROOT / 'dnsmasq.log').read_text()
assert private_daemon is None or private_daemon.poll() is None, (ROOT / 'dnsmasq.log').read_text()
(ROOT / 'server-ready.json').write_text(json.dumps({'pid': daemon.pid, 'legacyInterface': legacy,
                                                 'privateInterface': private, 'version': '2.91',
                                                 'authMode': auth_mode, 'authTargets': auth_targets,
                                                 'privatePid': private_daemon.pid if private_daemon else None}) + '\n')
try:
    while daemon.poll() is None:
        time.sleep(.1)
finally:
    for server in servers:
        server.shutdown()
    if daemon.poll() is None:
        daemon.terminate()
        daemon.wait(timeout=5)
    if private_daemon and private_daemon.poll() is None:
        private_daemon.terminate()
        private_daemon.wait(timeout=5)
