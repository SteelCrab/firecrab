use super::*;

#[test]
fn object_absence_keeps_real_docker_messages_distinct_from_inspection_failures() {
    for (kind, message) in [
        (
            "container",
            "Error response from daemon: No such container: owned",
        ),
        (
            "network",
            "Error response from daemon: network owned not found",
        ),
        (
            "volume",
            "Error response from daemon: get owned: no such volume",
        ),
        ("image", "Error response from daemon: No such image: owned"),
    ] {
        assert!(missing_object(message, kind, "owned"), "{message}");
    }
    for message in [
        "dial unix /var/run/docker.sock: no such file or directory",
        "permission denied while trying to connect to the Docker daemon",
        "Error response from daemon: network another not found",
    ] {
        assert!(!missing_object(message, "network", "owned"), "{message}");
    }
}

#[test]
fn endpoints_must_be_local_and_explicit_contexts_take_precedence() {
    for endpoint in [
        "unix:///var/run/docker.sock",
        "npipe:////./pipe/docker_engine",
        "tcp://localhost:2375",
        "tcp://127.0.0.1:2375",
        "tcp://[::1]:2375",
    ] {
        assert!(local_endpoint(endpoint), "{endpoint}");
    }
    for endpoint in [
        "ssh://host",
        "tcp://remote.example:2375",
        "tcp://192.168.1.5:2375",
        "https://localhost:2376",
        "unix://remote/socket",
        "npipe:////remote/pipe/docker_engine",
        "tcp://user:secret@localhost:2375",
        "unix:///tmp/socket\n",
    ] {
        assert!(!local_endpoint(endpoint), "{endpoint}");
    }
    assert!(
        endpoint_from(Some("ssh://remote"), None, |_| panic!(
            "remote host did not need context inspection"
        ))
        .is_err()
    );
    assert!(
        endpoint_from(Some("unix:///local"), Some("remote"), |context| {
            assert_eq!(context, Some("remote"));
            Ok("ssh://host".into())
        })
        .is_err()
    );
    assert_eq!(
        endpoint_from(None, None, |context| {
            assert!(context.is_none());
            Ok("unix:///var/run/docker.sock".into())
        })
        .unwrap(),
        "unix:///var/run/docker.sock"
    );
}

#[cfg(unix)]
mod fake {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    const PROGRAM: &str = r#"#!/usr/bin/env python3
import json, os, pathlib, sys
root=pathlib.Path(os.environ['FIRECRAB_DOCKER_FIXTURE'])
raw=sys.argv[1:]
assert raw[:2]==['--host','unix:///fixture/docker.sock'], raw
args=raw[2:]
event={'args':args,'host':raw[1]}
def record():
    with (root/'events').open('a') as f: f.write(json.dumps(event)+'\n')
def save(kind,name,value): (root/(kind+'.json')).write_text(json.dumps(value))
def labels(): return dict(arg.split('=',1) for i,arg in enumerate(args) if i and args[i-1]=='--label')
def option(name): return args[args.index(name)+1]
if args[0]=='info':
    value={'OSType':'linux','CgroupVersion':'2','Architecture':'x86_64','SecurityOptions':[]}
    if (root/'engine.json').exists(): value.update(json.loads((root/'engine.json').read_text()))
    record(); print(json.dumps(value)); sys.exit(0)
if len(args)>1 and args[1]=='inspect':
    path=root/(args[0]+'.json')
    record()
    if not path.exists():
        message = ('network '+args[-1]+' not found' if args[0]=='network' else
                   'get '+args[-1]+': no such volume' if args[0]=='volume' else
                   'No such '+args[0]+': '+args[-1])
        print('Error response from daemon: '+message,file=sys.stderr);sys.exit(1)
    print(path.read_text());sys.exit(0)
if args[0]=='build':
    event['context_files']=sorted(str(p.relative_to(args[-1])) for p in pathlib.Path(args[-1]).rglob('*') if p.is_file())
    save('image',option('--tag'),{'Config':{'Labels':labels()}})
elif args[:2] in [['network','create'],['volume','create']]:
    save(args[0],args[-1],{'Labels':labels(),'Driver':'bridge' if args[0]=='network' else 'local','Scope':'local','Options':None,'Containers':{}})
elif args[0]=='create':
    publication=option('--publish').split(':')
    volume=option('--mount').split('source=')[1].split(',')[0]
    devices=[{'PathOnHost':path,'PathInContainer':path,'CgroupPermissions':'rwm'} for path in ['/dev/kvm','/dev/net/tun']]
    value={'Config':{'Labels':labels()},'HostConfig':{'Privileged':False,'NetworkMode':option('--network'),'PidMode':'','UTSMode':'','UsernsMode':'','IpcMode':option('--ipc'),'CgroupnsMode':option('--cgroupns'),'CapAdd':['SYS_ADMIN','NET_ADMIN'],'Devices':devices,'Binds':[], 'PortBindings':{'5523/tcp':[{'HostIp':publication[0],'HostPort':publication[1]}]}},'NetworkSettings':{'Networks':{option('--network'):{}}},'Mounts':[{'Type':'volume','Name':volume,'Destination':'/var/lib/firecrab'}],'State':{'Running':False}}
    save('container',option('--name'),value)
elif args[0] in ['start','stop','restart']:
    value=json.loads((root/'container.json').read_text()); value['State']['Running']=args[0]!='stop';save('container',args[-1],value)
elif args[0]=='exec' and '-i' in args:
    if '--' in args:
        script=bytearray()
        while True:
            byte=sys.stdin.buffer.read(1)
            if not byte or byte==b'\0': break
            script.extend(byte)
        event['profile']=args[args.index('--')+1]
        event['script_size']=len(script)
    else:
        event['archive_size']=len(sys.stdin.buffer.read())
elif args[0]!='exec':
    raise AssertionError(args)
record()
"#;

    struct Fixture {
        root: tempfile::TempDir,
        _source: tempfile::TempDir,
        instance: Instance,
        docker: Docker,
        stop: Arc<AtomicBool>,
        api: Option<std::thread::JoinHandle<()>>,
    }

    impl Fixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let source = crate::micromanager::dev::tests::checkout_fixture();
            for file in [
                "install.sh",
                "scripts/firecrab-release.sh",
                "scripts/install-firecracker.sh",
            ] {
                fs::create_dir_all(source.path().join(file).parent().unwrap()).unwrap();
                fs::write(source.path().join(file), "fixture\n").unwrap();
            }
            let program = root.path().join("docker");
            fs::write(&program, PROGRAM).unwrap();
            fs::set_permissions(&program, fs::Permissions::from_mode(0o755)).unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let api_port = listener.local_addr().unwrap().port();
            listener.set_nonblocking(true).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let flag = stop.clone();
            let api = std::thread::spawn(move || {
                while !flag.load(Ordering::SeqCst) {
                    if let Ok((mut stream, _)) = listener.accept() {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(2)))
                            .unwrap();
                        let mut bytes = [0_u8; 2048];
                        let _ = stream.read(&mut bytes);
                        let _ = stream.write_all(
                            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}",
                        );
                    } else {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                }
            });
            let instance = Instance {
                id: "0123456789abcdef".into(),
                source: source.path().to_owned(),
                git_dir: root.path().join("git/worktrees/fixture"),
                primary_home: root.path().join("primary"),
                managed_home: root.path().join("private/micromanager"),
                install_dir: root.path().join("private/bin"),
                api_port,
                resource_port: api_port.saturating_sub(1),
            };
            let docker = Docker {
                program,
                endpoint: "unix:///fixture/docker.sock".into(),
                fixture: Some(root.path().to_owned()),
            };
            Self {
                root,
                _source: source,
                instance,
                docker,
                stop,
                api: Some(api),
            }
        }
        fn run(
            &self,
            command: Option<DevCommand>,
            release: bool,
            restore: bool,
        ) -> io::Result<i32> {
            run_with(
                &self.docker,
                &self.instance,
                command,
                release,
                restore,
                true,
            )
        }
        fn events(&self) -> Vec<Value> {
            fs::read_to_string(self.root.path().join("events"))
                .unwrap_or_default()
                .lines()
                .map(|line| serde_json::from_str(line).unwrap())
                .collect()
        }
        fn clear_events(&self) {
            fs::write(self.root.path().join("events"), "").unwrap();
        }
        fn change(&self, kind: &str, update: impl FnOnce(&mut Value)) {
            let path = self.root.path().join(format!("{kind}.json"));
            let mut value: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            update(&mut value);
            fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            self.api.take().unwrap().join().unwrap();
        }
    }

    #[test]
    fn source_build_uses_only_private_namespaces_storage_and_loopback_publication() {
        let fixture = Fixture::new();
        assert_eq!(fixture.run(None, false, false).unwrap(), 0);
        let events = fixture.events();
        let create = events
            .iter()
            .find(|event| event["args"][0] == "create")
            .unwrap();
        let args = create["args"].as_array().unwrap();
        let has_pair = |key: &str, value: &str| {
            args.windows(2)
                .any(|pair| pair[0] == key && pair[1] == value)
        };
        assert!(has_pair("--network", "firecrab-dev-0123456789abcdef"));
        assert!(has_pair("--cgroupns", "private"));
        assert!(has_pair("--ipc", "private"));
        assert!(has_pair("--device", "/dev/kvm:/dev/kvm:rwm"));
        assert!(has_pair("--device", "/dev/net/tun:/dev/net/tun:rwm"));
        assert!(has_pair(
            "--publish",
            &format!("127.0.0.1:{}:5523/tcp", fixture.instance.api_port)
        ));
        assert!(!args.iter().any(|arg| arg == "--privileged"
            || arg == "--pid"
            || arg == "host"
            || arg == "--volume"));
        assert_eq!(args.iter().filter(|arg| *arg == "--publish").count(), 1);
        let build = events
            .iter()
            .find(|event| event["args"][0] == "build")
            .unwrap();
        assert!(
            build["context_files"]
                .as_array()
                .unwrap()
                .contains(&Value::String("dev-container-entrypoint.sh".into()))
        );
        assert!(
            !build["context_files"]
                .as_array()
                .unwrap()
                .contains(&Value::String("Cargo.toml".into()))
        );
        let upload = events
            .iter()
            .position(|event| event["archive_size"].as_u64().is_some_and(|size| size > 0))
            .unwrap();
        let deploy = events
            .iter()
            .position(|event| event["profile"] == "debug")
            .unwrap();
        assert!(upload < deploy);
        assert!(events[deploy]["script_size"].as_u64().unwrap() > 0);
        assert!(
            events
                .iter()
                .all(|event| event["host"] == "unix:///fixture/docker.sock")
        );
        assert!(!fixture.instance.primary_home.exists());
    }

    #[test]
    fn lifecycle_and_restore_never_remove_data_or_touch_other_services() {
        let fixture = Fixture::new();
        fixture.run(None, true, false).unwrap();
        assert!(
            fixture
                .events()
                .iter()
                .any(|event| event["profile"] == "release")
        );
        for command in [
            DevCommand::Stop,
            DevCommand::Start,
            DevCommand::Restart,
            DevCommand::Clean,
        ] {
            fixture.clear_events();
            fixture.run(Some(command), false, false).unwrap();
            let events = fixture.events();
            if command == DevCommand::Stop {
                assert!(events.iter().any(|event| event["args"][0] == "stop"));
                assert!(!events.iter().any(|event| event["args"][0] == "exec"));
            } else {
                assert!(
                    events
                        .iter()
                        .any(|event| event["profile"] == command.as_str())
                );
            }
            assert!(
                !events
                    .iter()
                    .any(
                        |event| event["args"].as_array().unwrap().iter().any(|arg| matches!(
                            arg.as_str(),
                            Some("prune" | "rm" | "wsl.exe" | "systemctl")
                        ))
                    )
            );
            assert!(
                !events
                    .iter()
                    .any(|event| event.get("archive_size").is_some())
            );
        }
        fixture.clear_events();
        fixture.run(None, false, true).unwrap();
        assert!(
            fixture
                .events()
                .iter()
                .any(|event| event["profile"] == "restore")
        );
        assert!(
            !fixture
                .events()
                .iter()
                .any(|event| event["args"][0] == "build")
        );
    }

    #[test]
    fn a_busy_instance_rejects_control_before_contacting_docker() {
        let fixture = Fixture::new();
        let lock = container_lock(&fixture.instance.managed_home).unwrap();
        let error = fixture
            .run(Some(DevCommand::Stop), false, false)
            .unwrap_err();
        assert!(error.to_string().contains("another development operation"));
        assert!(fixture.events().is_empty());
        fs2::FileExt::unlock(&lock).unwrap();
        fixture.run(Some(DevCommand::Stop), false, false).unwrap();
    }

    #[test]
    fn missing_instances_stop_without_creation_and_start_fails_closed() {
        let fixture = Fixture::new();
        fixture.run(Some(DevCommand::Stop), false, false).unwrap();
        fixture.run(Some(DevCommand::Clean), false, false).unwrap();
        assert!(fixture.run(Some(DevCommand::Start), false, false).is_err());
        assert!(
            !fixture
                .events()
                .iter()
                .any(|event| event["args"][0] == "create" || event["args"][0] == "build")
        );
    }

    #[test]
    fn foreign_ownership_shared_namespaces_and_bind_backed_volumes_are_rejected() {
        for kind in ["container", "network", "volume"] {
            let fixture = Fixture::new();
            fixture.run(None, false, false).unwrap();
            fixture.change(kind, |value| {
                let labels = if kind == "container" {
                    &mut value["Config"]["Labels"]
                } else {
                    &mut value["Labels"]
                };
                labels[OWNER] = "another-worktree".into();
            });
            fixture.clear_events();
            assert!(fixture.run(Some(DevCommand::Stop), false, false).is_err());
            assert!(
                !fixture
                    .events()
                    .iter()
                    .any(|event| event["args"][0] == "stop")
            );
        }
        let fixture = Fixture::new();
        fixture.run(None, false, false).unwrap();
        fixture.change("container", |value| {
            value["HostConfig"]["PidMode"] = "host".into()
        });
        assert!(
            fixture
                .run(Some(DevCommand::Restart), false, false)
                .is_err()
        );
        fixture.change("container", |value| {
            value["HostConfig"]["PidMode"] = "".into()
        });
        fixture.change("container", |value| {
            value["HostConfig"]["UTSMode"] = "host".into()
        });
        assert!(fixture.run(Some(DevCommand::Start), false, false).is_err());
        fixture.change("container", |value| {
            value["HostConfig"]["UTSMode"] = "".into()
        });
        fixture.change("container", |value| {
            value["HostConfig"]["Devices"][0]["PathOnHost"] = "/dev/sda".into()
        });
        assert!(fixture.run(Some(DevCommand::Start), false, false).is_err());
        fixture.change("container", |value| {
            value["HostConfig"]["Devices"][0]["PathOnHost"] = "/dev/kvm".into()
        });
        fixture.change("container", |value| {
            value["NetworkSettings"]["Networks"]["another-worktree"] = serde_json::json!({});
        });
        assert!(fixture.run(Some(DevCommand::Start), false, false).is_err());
        fixture.change("container", |value| {
            value["NetworkSettings"]["Networks"]
                .as_object_mut()
                .unwrap()
                .remove("another-worktree");
        });
        fixture.change("volume", |value| {
            value["Options"] = serde_json::json!({"type":"none","o":"bind","device":"/"})
        });
        assert!(fixture.run(Some(DevCommand::Start), false, false).is_err());
    }

    #[test]
    fn a_missing_private_data_mount_rejects_control_before_stopping_the_container() {
        let fixture = Fixture::new();
        fixture.run(None, false, false).unwrap();
        fixture.change("container", |value| value["Mounts"] = serde_json::json!([]));
        fixture.clear_events();
        assert!(fixture.run(Some(DevCommand::Stop), false, false).is_err());
        assert!(
            !fixture
                .events()
                .iter()
                .any(|event| event["args"][0] == "stop")
        );
    }

    #[test]
    fn apparmor_override_is_used_only_when_the_engine_supports_apparmor() {
        for enabled in [false, true] {
            let fixture = Fixture::new();
            let options: Vec<&str> = if enabled {
                vec!["name=apparmor"]
            } else {
                vec![]
            };
            fs::write(
                fixture.root.path().join("engine.json"),
                serde_json::json!({"SecurityOptions":options}).to_string(),
            )
            .unwrap();
            fixture.run(None, false, false).unwrap();
            let events = fixture.events();
            let create = events
                .iter()
                .find(|event| event["args"][0] == "create")
                .unwrap();
            assert_eq!(
                create["args"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|arg| arg == "apparmor=unconfined"),
                enabled
            );
        }
    }

    #[test]
    fn unsupported_engines_fail_before_creating_resources() {
        for engine in [
            serde_json::json!({"OSType":"windows"}),
            serde_json::json!({"CgroupVersion":"1"}),
            serde_json::json!({"SecurityOptions":["name=rootless"]}),
        ] {
            let fixture = Fixture::new();
            fs::write(fixture.root.path().join("engine.json"), engine.to_string()).unwrap();
            assert!(fixture.run(None, false, false).is_err());
            assert!(
                fixture
                    .events()
                    .iter()
                    .all(|event| event["args"][0] == "info")
            );
        }
    }
}
