"""One bounded framed stdin and a real guest PTY for the saved workspace user."""
import base64
import fcntl
import pty
import select
import signal
import struct
import termios


def terminal(payload):
    user = identity(payload)
    child, master = pty.fork()
    if child == 0:
        drop(user)
        os.chdir('/home/fc-code/project')
        os.execve('/bin/sh', ['/bin/sh', '-i'], {'HOME': user.pw_dir, 'USER': user.pw_name,
                  'LOGNAME': user.pw_name, 'PATH': '/usr/local/bin:/usr/bin:/bin', 'SHELL': '/bin/sh',
                  'TERM': 'xterm-256color', 'LANG': 'C.UTF-8'})
    pending = bytearray()
    try:
        while True:
            readable, _, _ = select.select([0, master], [], [], 3600)
            if not readable:
                break
            if master in readable:
                try:
                    data = os.read(master, 8192)
                except OSError:
                    break
                if not data:
                    break
                while data:
                    written = os.write(1, data)
                    data = data[written:]
            if 0 in readable:
                data = os.read(0, 32768)
                if not data:
                    break
                pending.extend(data)
                while len(pending) >= 4:
                    length = struct.unpack('!I', pending[:4])[0]
                    if length > 65536:
                        raise RuntimeError('Terminal frame too large')
                    if len(pending) < length + 4:
                        break
                    message = json.loads(pending[4:length + 4])
                    del pending[:length + 4]
                    if set(message) == {'input'}:
                        data = base64.b64decode(message['input'], validate=True)
                        if len(data) > 16384:
                            raise RuntimeError('Terminal input too large')
                        while data:
                            written = os.write(master, data)
                            data = data[written:]
                    elif set(message) == {'rows', 'cols'} and all(type(message[k]) is int and 2 <= message[k] <= 1000 for k in message):
                        fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack('HHHH', message['rows'], message['cols'], 0, 0))
                    else:
                        raise RuntimeError('Invalid terminal control')
    finally:
        os.close(master)
        try:
            os.killpg(child, signal.SIGHUP)
        except ProcessLookupError:
            pass
        os.waitpid(child, 0)
