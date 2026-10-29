import fcntl
import json
from pathlib import Path
import random
import socket
import struct
import time

ROOT = Path('/qa')


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


def dhcp(iface, address):
    mac = bytes.fromhex(Path('/sys/class/net/' + iface + '/address').read_text().strip().replace(':', ''))
    xid = random.getrandbits(32)
    bootp = struct.pack('!BBBBIHHIIII16s64s128s', 1, 1, 6, 0, xid, 0, 0,
                        0, 0, 0, 0, mac + b'\0' * 10, b'', b'') + b'\x63\x82\x53\x63'
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as sock:
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_BROADCAST, 1)
        sock.setsockopt(socket.SOL_SOCKET, socket.SO_BINDTODEVICE, iface.encode() + b'\0')
        sock.bind(('0.0.0.0', 68))
        sock.settimeout(5)
        def receive(expected):
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                packet, peer = sock.recvfrom(4096)
                if len(packet) < 240 or struct.unpack('!I', packet[4:8])[0] != xid:
                    continue
                options = {}
                offset = 240
                while offset < len(packet):
                    code = packet[offset]
                    offset += 1
                    if code == 255:
                        break
                    if code == 0:
                        continue
                    size = packet[offset]
                    offset += 1
                    options[code] = packet[offset:offset + size]
                    offset += size
                if options.get(53) == bytes([expected]):
                    return packet, options, peer
            raise TimeoutError('DHCP')
        sock.sendto(bootp + b'\x35\x01\x01\x3d\x07\x01' + mac + b'\x37\x03\x01\x03\x06\xff',
                    (address.rsplit('.', 1)[0] + '.2', 67))
        offered, options, peer = receive(2)
        requested = offered[16:20]
        assert socket.inet_ntoa(requested) == address
        sock.sendto(bootp + b'\x35\x01\x03\x32\x04' + requested + b'\x36\x04' + options[54] +
                    b'\x3d\x07\x01' + mac + b'\xff', (address.rsplit('.', 1)[0] + '.2', 67))
        acknowledged, _, _ = receive(5)
        assert acknowledged[16:20] == requested
        return {'address': address, 'server': peer[0], 'offer': True, 'ack': True}


def question(name, qtype, identifier=42, recursive=True):
    encoded = b''.join(bytes([len(part)]) + part.encode() for part in name.split('.')) + b'\0'
    return struct.pack('!HHHHHH', identifier, 0x100 if recursive else 0, 1, 0, 0, 0) + encoded + struct.pack('!HH', qtype, 1)


def read_exact(sock, size):
    output = b''
    while len(output) < size:
        part = sock.recv(size - len(output))
        if not part:
            raise EOFError('DNS TCP closed')
        output += part
    return output


class Query:
    def __init__(self, address, transport):
        self.transport = transport
        family = socket.AF_INET6 if ':' in address else socket.AF_INET
        self.sock = socket.socket(family, socket.SOCK_DGRAM if transport == 'udp' else socket.SOCK_STREAM)
        self.sock.settimeout(3)
        self.sock.connect((address, 53))
        if transport == 'tcp':
            self.sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)

    def ask(self, name, qtype, identifier=42, recursive=True):
        packet = question(name, qtype, identifier, recursive)
        if self.transport == 'udp':
            self.sock.send(packet)
            reply = self.sock.recv(65535)
        else:
            self.sock.sendall(struct.pack('!H', len(packet)) + packet)
            reply = read_exact(self.sock, struct.unpack('!H', read_exact(self.sock, 2))[0])
        header = struct.unpack('!HHHHHH', reply[:12])
        assert header[0] == identifier, header
        return {'rcode': header[1] & 15, 'answers': header[3], 'authorities': header[4],
                'recursiveAvailable': bool(header[1] & 0x80), 'bytes': len(reply)}

    def close(self):
        self.sock.close()


out = {'kind': 'dnsmasq_2_91_linux_fixture', 'microvmValidated': False, 'dhcp': {}, 'warmLegacy': [],
       'privateKnownTypes': [], 'internal': [], 'exhaustive': []}
try:
    for ip, label, assigned in [('10.241.41.3', 'legacy', '10.241.41.3'),
                                ('10.241.42.3', 'private', '10.241.42.3')]:
        out['dhcp'][label] = dhcp(interface(ip), assigned)
        print('DHCP', label, 'PASS', flush=True)
    types = [1, 28, 16, 2, 5, 12, 15]
    legacy = Query('10.241.41.2', 'udp')
    for qtype in types:
        result = legacy.ask('cached.external.test', qtype)
        assert result['answers'] > 0, result
        out['warmLegacy'].append({'type': qtype, **result})
    before = json.loads((ROOT / 'upstream-count.json').read_text())['count']
    for address in ['10.241.42.2', 'fd12:414:42::2']:
        for transport in ['udp', 'tcp']:
            query = Query(address, transport)
            for qtype in types + [6, 33, 35, 43, 46, 48, 64, 65, 249, 250, 251, 252, 255, 257, 65534, 65535]:
                for recursive in [False, True]:
                    result = query.ask('cached.external.test', qtype, recursive=recursive)
                    assert result['answers'] == 0, result
                    out['privateKnownTypes'].append({'address': address, 'transport': transport,
                                                    'type': qtype, 'rd': recursive, **result})
            for name in ['backend.internal', 'private-client.internal', 'private.internal']:
                result = query.ask(name, 1)
                assert result['rcode'] == 0 and result['answers'] > 0, (name, result)
                out['internal'].append({'address': address, 'transport': transport, 'name': name, **result})
            query.close()
            print('known-types/internal', address, transport, 'PASS', flush=True)
    assert json.loads((ROOT / 'upstream-count.json').read_text())['count'] == before
    for address in ['10.241.42.2', 'fd12:414:42::2']:
        for transport in ['udp', 'tcp']:
            query = Query(address, transport)
            started = time.monotonic()
            for qtype in range(65536):
                if transport == 'tcp' and qtype and qtype % 32 == 0:
                    query.close()
                    query = Query(address, transport)
                result = query.ask('cached.external.test', qtype, identifier=qtype)
                assert result['answers'] == 0, (qtype, result)
                if qtype % 8192 == 0:
                    print('all-types', address, transport, qtype, flush=True)
            query.close()
            out['exhaustive'].append({'address': address, 'transport': transport, 'types': 65536,
                                      'seconds': time.monotonic() - started})
            assert json.loads((ROOT / 'upstream-count.json').read_text())['count'] == before
    # A new legacy name still forwards after all private traffic, and a repeat
    # uses the legacy cache. Authoritative mode did not disable legacy recursion.
    assert legacy.ask('after.external.test', 1)['answers'] > 0
    after = json.loads((ROOT / 'upstream-count.json').read_text())['count']
    assert after > before
    assert legacy.ask('after.external.test', 1)['answers'] > 0
    assert json.loads((ROOT / 'upstream-count.json').read_text())['count'] == after
    legacy.close()
    out['upstreamBeforePrivate'] = before
    out['upstreamAfterPrivate'] = before
    out['upstreamAfterNewLegacy'] = after
    out['status'] = 'passed'
except BaseException as error:
    out['status'] = 'failed'
    out['error'] = repr(error)
    raise
finally:
    (ROOT / 'result.json').write_text(json.dumps(out, indent=2) + '\n')
