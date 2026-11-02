#!/usr/bin/env python3
"""Real bounded commands in an isolated root Linux fixture; no host/runtime service."""
import os
from pathlib import Path
import signal
import struct
import subprocess
import sys
import tempfile
import time
import unittest
import uuid

REPO = Path(__file__).resolve().parent.parent

@unittest.skipUnless(sys.platform == 'linux' and os.geteuid() == 0, 'requires an isolated Linux root fixture')
class Commands(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.build = tempfile.TemporaryDirectory(prefix='firecrab-command-build-')
        cls.binary = Path(cls.build.name) / 'commands'
        driver = Path(cls.build.name) / 'main.c'
        driver.write_text('#include "workspace.h"\nint main(int n,char **v){return firecrab_workspace_command(n,v);}\n')
        source = REPO / 'packaging/guest-console'
        subprocess.run([os.environ.get('CC','cc'), '-std=c11', '-Wall', '-Wextra', '-Werror',
            '-DFIRECRAB_WORKSPACE_TEST', '-I', str(source), str(source/'workspace.c'),
            str(source/'workspace_process.c'), str(source/'command.c'), str(driver), '-o', str(cls.binary)], check=True)
    @classmethod
    def tearDownClass(cls): cls.build.cleanup()
    def setUp(self):
        self.fixture = tempfile.TemporaryDirectory(prefix='firecrab-command-')
        self.root = Path(self.fixture.name)
        self.project,self.database,self.volume=[str(uuid.uuid4()) for _ in range(3)]
        self.digest='a'*64
        marker=self.root/'.firecrab-workspace'; marker.mkdir(mode=0o700)
        (marker/'ready').write_text(f'{self.project}:{self.database}:{self.digest}')
        (marker/'ready').chmod(0o600)
        self.commands=[]; self.addCleanup(self.cleanup)
    def cleanup(self):
        for identity, request in self.commands:
            try:
                self.call(identity, request, 3)
                self.until(lambda: self.call(identity, request)['state'] in (2,3))
            except (AssertionError,OSError,subprocess.SubprocessError): pass
        self.fixture.cleanup()
    def payload(self, argv=None, cwd='', env=(), stdin=b'', wall=5, cpu=3, memory=64, output=8192):
        args=[s.encode() for s in (argv or ['/bin/sh','-c','printf completed'])]
        env=[s.encode() for s in env]; cwd=cwd.encode()
        return struct.pack('>8I',wall,cpu,memory,output,len(cwd),len(args),len(env),len(stdin))+cwd+\
            b''.join(struct.pack('>I',len(s))+s for s in args+env)+stdin
    def call(self, identity, request=b'', action=2, error=None):
        frame=b'FCX1'+struct.pack('>I',action)+identity.bytes+struct.pack('>I',len(request))+request
        p=subprocess.run([str(self.binary),'--workspace-command',self.project,self.database,self.volume,self.digest,str(self.root)],
            input=frame,capture_output=True,timeout=12,check=True)
        magic=p.stdout[:4]; n=struct.unpack('>I',p.stdout[4:8])[0]
        if error is not None:
            self.assertEqual((magic,n),(b'FCXE',error),p.stdout); return
        self.assertIn(magic,(b'FCXR',b'FCXP'),p.stdout)
        self.assertEqual(len(p.stdout),n+8)
        if magic==b'FCXP': self.assertEqual(p.stdout[8:],struct.pack('>I',1));return
        m=p.stdout[8:80]
        self.assertEqual(m[:4],b'FCXS')
        state,uid,gid,supervisor,child=struct.unpack('>5I',m[4:24])
        exit,flags,out,err=struct.unpack('>4I',m[56:72])
        self.assertEqual(len(p.stdout),80+out+err)
        return dict(state=state,uid=uid,gid=gid,supervisor=supervisor,child=child,
            exit=exit,flags=flags,stdout=p.stdout[80:80+out],stderr=p.stdout[80+out:])
    def until(self, work, seconds=10):
        deadline=time.monotonic()+seconds
        while time.monotonic()<deadline:
            value=work()
            if value:return value
            time.sleep(.03)
        self.fail('native command did not settle')
    def start(self, **kwargs):
        identity=uuid.uuid4(); request=self.payload(**kwargs)
        self.commands.append((identity,request)); self.call(identity,request,1)
        return identity,request
    def finish(self, identity, request):
        return self.until(lambda: (r if (r:=self.call(identity,request))['state']==2 else None))
    def test_literal_arguments_stdin_environment_and_distinct_streams(self):
        identity,request=self.start(argv=['/bin/sh','-c','printf "%s:%s:" "$1" "$PROOF"; cat; printf err >&2; exit 7','sh','$(touch injected)'],env=['PROOF=explicit'],stdin=b'input\x00bytes')
        r=self.finish(identity,request)
        self.assertEqual(r['stdout'],b'$(touch injected):explicit:input\x00bytes')
        self.assertEqual(r['stderr'],b'err'); self.assertEqual(r['exit'],7)
        self.assertFalse((self.root/'injected').exists())
    def test_private_metadata_unprivileged_identity_and_enforced_limits(self):
        identity,request=self.start(argv=['/bin/sh','-c','id -u; grep NoNewPrivs /proc/self/status; cat /proc/self/limits; env; cat .firecrab-workspace/ready'],env=['ONLY_USER_SETTING=present'])
        r=self.finish(identity,request)
        self.assertGreaterEqual(r['uid'],100000)
        self.assertIn(str(r['uid']).encode(),r['stdout']); self.assertIn(b'NoNewPrivs:\t1',r['stdout'])
        self.assertIn(b'Max cpu time',r['stdout']); self.assertIn(b'67108864',r['stdout'])
        self.assertNotIn(b'HOSTNAME=',r['stdout']); self.assertNotIn(self.digest.encode(),r['stdout'])
        self.assertIn(b'Permission denied',r['stderr'])
    def test_retained_identity_never_reexecutes_and_rejects_changed_payload(self):
        identity,request=self.start(argv=['/bin/sh','-c','printf once >> starts; printf output'])
        first=self.finish(identity,request)
        repeated=self.call(identity,request,1)
        self.assertEqual(repeated,first); self.assertEqual((self.root/'starts').read_bytes(),b'once')
        self.call(identity,self.payload(argv=['/bin/sh','-c','printf duplicate >> starts']),1,error=3)
        self.assertEqual((self.root/'starts').read_bytes(),b'once')
    def test_working_directory_and_missing_queries_do_not_launch(self):
        (self.root/'api').mkdir()
        identity,request=self.start(cwd='api',argv=['/bin/sh','-c','printf scoped > result'])
        self.finish(identity,request);self.assertEqual((self.root/'api/result').read_bytes(),b'scoped')
        self.call(uuid.uuid4(),self.payload(),2,error=2)
        self.call(uuid.uuid4(),self.payload(cwd='../'),1,error=1)
        (self.root/'escape').symlink_to('/tmp',target_is_directory=True)
        self.call(uuid.uuid4(),self.payload(cwd='escape'),1,error=5)
    def test_output_caps_and_wall_deadline_stop_unbounded_commands(self):
        identity,request=self.start(argv=['/bin/sh','-c','yes output;'],wall=1,output=64)
        r=self.finish(identity,request)
        self.assertEqual(r['exit'],124);self.assertEqual(len(r['stdout']),64)
        self.assertTrue(r['flags']&2);self.assertTrue(r['flags']&16);self.assertFalse(r['flags']&1)
    def test_cancellation_kills_detached_descendants_without_stopping_another_command(self):
        a,ar=self.start(argv=['/bin/sh','-c','setsid sleep 90 & printf ready; wait'],wall=90)
        b,br=self.start(argv=['/bin/sh','-c','sleep 2; printf sibling'],wall=10)
        self.until(lambda:b'ready' in self.call(a,ar)['stdout'])
        self.call(a,ar,3);r=self.finish(a,ar)
        self.assertEqual(r['exit'],143);self.assertTrue(r['flags']&8);self.assertFalse(r['flags']&1)
        self.assertEqual(self.finish(b,br)['stdout'],b'sibling')
    def test_supervisor_loss_retains_partial_output_and_requires_explicit_cleanup(self):
        identity,request=self.start(argv=['/bin/sh','-c','printf partial; sleep 90'],wall=90)
        r=self.until(lambda:(v if b'partial' in (v:=self.call(identity,request))['stdout'] else None))
        os.kill(r['supervisor'],signal.SIGKILL)
        r=self.until(lambda:(v if (v:=self.call(identity,request))['state']==3 else None))
        self.assertEqual(r['stdout'],b'partial');self.assertTrue(r['flags']&1)
        r=self.call(identity,request,3);self.assertFalse(r['flags']&1)
        self.assertEqual(r['exit'],0xffffffff)
        self.assertEqual(self.call(identity,request,1)['state'],3)
    def test_cancel_before_arrival_fences_a_late_launch(self):
        identity=uuid.uuid4();request=self.payload(argv=['/bin/sh','-c','printf forbidden > late'])
        r=self.call(identity,request,3);self.assertEqual(r['state'],2);self.assertEqual(r['exit'],143)
        r=self.call(identity,request,1);self.assertEqual(r['child'],0)
        self.assertFalse((self.root/'late').exists())
    def test_old_boot_receipt_cannot_signal_a_reused_uid(self):
        a,ar=self.start(argv=['/bin/sh','-c','printf old'])
        self.finish(a,ar)
        b,br=self.start(argv=['/bin/sh','-c','sleep 1; printf current'],wall=5)
        sibling=self.call(b,br)
        path=self.root/'.firecrab-workspace/commands'/a.hex/'state'
        for state in (1,3):
            m=bytearray(path.read_bytes());m[4:8]=struct.pack('>I',state);m[8:12]=struct.pack('>I',sibling['uid'])
            m[40:56]=uuid.uuid4().bytes;m[56:60]=struct.pack('>I',0xffffffff)
            m[60:64]=struct.pack('>I',1);path.write_bytes(m)
            r=self.call(a,ar,3);self.assertEqual(r['state'],3);self.assertFalse(r['flags']&1)
        self.assertEqual(self.finish(b,br)['stdout'],b'current')
    def test_interrupted_launch_is_retained_without_inventing_an_execution(self):
        identity=uuid.uuid4();request=self.payload()
        self.call(identity,b'',4)
        parent=self.root/'.firecrab-workspace/commands';parent.mkdir(mode=0o711)
        folder=parent/identity.hex;folder.mkdir(mode=0o711)
        (folder/'request').write_bytes(request);(folder/'request').chmod(0o600)
        boot=uuid.UUID(Path('/proc/sys/kernel/random/boot_id').read_text().strip()).bytes
        meta=b'FCXS'+struct.pack('>5I2Q',0,900000001,2000000000,0,0,0,0)+boot+struct.pack('>4I',0xffffffff,0,0,0)
        (folder/'state').write_bytes(meta);(folder/'state').chmod(0o600)
        r=self.call(identity,request,1);self.assertEqual(r['state'],3)
        self.assertEqual(r['child'],0);self.assertTrue(r['flags']&1)

if __name__=='__main__': unittest.main()
