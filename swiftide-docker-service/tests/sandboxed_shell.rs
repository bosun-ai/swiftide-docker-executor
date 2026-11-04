//! Runs real commands under the real sandbox, through the real service binary and its gRPC API.
//! A mocked sandbox proves nothing, so these tests need a kernel with Landlock ABI 4 or later and
//! deliberately fail, rather than skip, without one.
//!
//! `bash`, `git` and `python3` must be on the path.

use std::collections::HashMap;
use std::fs;
use std::io::{ErrorKind, Write as _};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::os::fd::AsRawFd as _;
use std::os::unix::fs::PermissionsExt as _;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;

use tempfile::TempDir;
use tonic::{Code, Status};

#[allow(clippy::double_must_use)]
mod codegen {
    tonic::include_proto!("shell");
}

use codegen::shell_event::Event;
use codegen::shell_executor_client::ShellExecutorClient;
use codegen::{ShellEvent, ShellRequest};

/// The real service binary, listening on its own address for the length of a test.
struct Service {
    process: Child,
    ip: Ipv4Addr,
}

impl Service {
    const PORT: u16 = 39517;

    async fn start() -> Self {
        Self::start_with(Command::new(env!("CARGO_BIN_EXE_swiftide-docker-service"))).await
    }

    /// A service whose sandboxed shells write to `directory`, so that a test can plant things
    /// there without affecting tests running alongside it.
    async fn with_sandbox_directory(directory: &Path) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_swiftide-docker-service"));
        command.env("SWIFTIDE_DOCKER_SERVICE_SANDBOX_DIR", directory);
        Self::start_with(command).await
    }

    async fn start_with(mut command: Command) -> Self {
        let ip = unique_loopback_address();
        let process = command
            .env(
                "SWIFTIDE_DOCKER_SERVICE_ADDR",
                format!("{ip}:{}", Self::PORT),
            )
            .stdout(Stdio::null())
            .spawn()
            .expect("service should start");
        let service = Self { process, ip };
        service.wait_until_listening().await;
        service
    }

    async fn wait_until_listening(&self) {
        for _ in 0..200 {
            if TcpStream::connect((self.ip, Self::PORT)).is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("service did not start listening");
    }

    async fn client(&self) -> ShellExecutorClient<tonic::transport::Channel> {
        ShellExecutorClient::connect(format!("http://{}:{}", self.ip, Self::PORT))
            .await
            .expect("service should accept connections")
    }

    async fn normal(&self, request: ShellRequest) -> Result<Outcome, Status> {
        let events = self.client().await.exec_shell(request).await?;
        Outcome::collect(events.into_inner()).await
    }

    async fn sandboxed(&self, request: ShellRequest) -> Result<Outcome, Status> {
        let events = self.client().await.exec_sandboxed_shell(request).await?;
        Outcome::collect(events.into_inner()).await
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

/// All of 127.0.0.0/8 is local, so tests running in parallel, in threads or in processes, need
/// not race for free ports.
fn unique_loopback_address() -> Ipv4Addr {
    static NEXT: AtomicU8 = AtomicU8::new(1);
    let [pid_high, pid_low] = (std::process::id() as u16).to_be_bytes();
    Ipv4Addr::new(127, pid_high, pid_low, NEXT.fetch_add(1, Ordering::Relaxed))
}

#[derive(Debug)]
struct Outcome {
    stdout: String,
    stderr: String,
    result: Event,
}

impl Outcome {
    async fn collect(mut events: tonic::Streaming<ShellEvent>) -> Result<Self, Status> {
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        while let Some(event) = events.message().await? {
            match event.event.expect("event should have a payload") {
                Event::Stdout(bytes) => stdout.extend_from_slice(&bytes),
                Event::Stderr(bytes) => stderr.extend_from_slice(&bytes),
                result @ (Event::ExitCode(_) | Event::TimedOutAfterMs(_)) => {
                    return Ok(Self {
                        stdout: String::from_utf8_lossy(&stdout).into_owned(),
                        stderr: String::from_utf8_lossy(&stderr).into_owned(),
                        result,
                    });
                }
            }
        }
        panic!("the stream should end with a result");
    }

    #[track_caller]
    fn assert_succeeded(&self) {
        assert_eq!(self.result, Event::ExitCode(0), "{self:#?}");
    }

    #[track_caller]
    fn assert_failed(&self) {
        assert!(
            matches!(self.result, Event::ExitCode(code) if code != 0),
            "{self:#?}"
        );
    }
}

/// A checkout and a home directory outside /tmp, an agent's view of the executor.
struct Workspace {
    _root: TempDir,
    checkout: PathBuf,
    home: PathBuf,
}

impl Workspace {
    fn new() -> Self {
        let root = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        assert!(
            !root.path().starts_with("/tmp"),
            "the workspace must be outside the writable /tmp; move the cargo target directory"
        );
        let checkout = root.path().join("checkout");
        let home = root.path().join("home");
        fs::create_dir(&checkout).unwrap();
        fs::create_dir(&home).unwrap();
        fs::write(checkout.join("existing.txt"), "original\n").unwrap();
        fs::write(home.join("existing.txt"), "original\n").unwrap();
        Self {
            _root: root,
            checkout,
            home,
        }
    }

    fn request(&self, command: impl Into<String>) -> ShellRequest {
        ShellRequest {
            command: command.into(),
            envs: HashMap::from([("HOME".into(), self.home.display().to_string())]),
            timeout_ms: Some(30_000),
            cwd: Some(self.checkout.display().to_string()),
            ..Default::default()
        }
    }

    fn checkout_entries(&self) -> Vec<String> {
        let mut entries: Vec<_> = fs::read_dir(&self.checkout)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        entries.sort();
        entries
    }

    #[track_caller]
    fn assert_checkout_untouched(&self) {
        assert_eq!(self.checkout_entries(), ["existing.txt"]);
        assert_eq!(
            fs::read_to_string(self.checkout.join("existing.txt")).unwrap(),
            "original\n"
        );
    }
}

async fn eventually(what: &str, condition: impl Fn() -> bool) {
    for _ in 0..200 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("timed out waiting until {what}");
}

fn python(script: &str) -> String {
    assert!(!script.contains('\''), "the script is quoted for the shell");
    format!("python3 -c '{script}'")
}

#[tokio::test]
async fn reads_files_and_lists_directories() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    let outcome = service
        .sandboxed(workspace.request("cat existing.txt; ls; cat /etc/hostname > /dev/null"))
        .await
        .unwrap();

    outcome.assert_succeeded();
    assert_eq!(outcome.stdout, "original\nexisting.txt\n");
}

#[tokio::test]
async fn git_log_and_git_diff_read_a_repository() {
    let service = Service::start().await;
    let workspace = Workspace::new();
    service
        .normal(workspace.request(
            "git init -q && git add . && \
             git -c user.name=t -c user.email=t@example.com commit -qm 'the first commit' && \
             echo changed > existing.txt",
        ))
        .await
        .unwrap()
        .assert_succeeded();

    let log = service
        .sandboxed(workspace.request("git log --format=%s"))
        .await
        .unwrap();
    log.assert_succeeded();
    assert_eq!(log.stdout, "the first commit\n");

    let diff = service
        .sandboxed(workspace.request("git diff"))
        .await
        .unwrap();
    diff.assert_succeeded();
    assert!(diff.stdout.contains("-original"), "{diff:#?}");
    assert!(diff.stdout.contains("+changed"), "{diff:#?}");
}

#[tokio::test]
async fn writes_to_the_checkout_are_denied() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    for (write, command) in [
        ("create a file", "echo new > new.txt"),
        ("create with touch", "touch new.txt"),
        ("append", "echo more >> existing.txt"),
        ("overwrite", "echo over > existing.txt"),
        ("truncate by redirect", ": > existing.txt"),
        ("truncate with coreutils", "truncate -s 0 existing.txt"),
        ("remove", "rm existing.txt"),
        ("rename", "mv existing.txt renamed.txt"),
        ("make a directory", "mkdir new"),
        ("make a symlink", "ln -s existing.txt link"),
        ("make a fifo", "mkfifo fifo"),
        ("edit in place", "sed -i s/original/edited/ existing.txt"),
        (
            "reach it through /proc",
            "echo new > /proc/self/root$PWD/new.txt",
        ),
        (
            "reach it through the service in /proc",
            "echo new > /proc/$PPID/root$PWD/new.txt",
        ),
        (
            "write to the service's memory",
            "echo new > /proc/$PPID/mem",
        ),
    ] {
        let outcome = service.sandboxed(workspace.request(command)).await.unwrap();
        assert!(
            matches!(outcome.result, Event::ExitCode(code) if code != 0),
            "{write} should fail: {outcome:#?}"
        );
        workspace.assert_checkout_untouched();
    }
}

#[tokio::test]
async fn truncate_syscall_on_an_existing_file_is_denied() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    let outcome = service
        .sandboxed(workspace.request(python(r#"import os; os.truncate("existing.txt", 0)"#)))
        .await
        .unwrap();

    outcome.assert_failed();
    assert!(outcome.stderr.contains("Permission denied"), "{outcome:#?}");
    workspace.assert_checkout_untouched();
}

#[tokio::test]
async fn writes_to_home_are_denied() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    for command in [
        "echo new > ~/new.txt",
        ": > ~/existing.txt",
        "truncate -s 0 ~/existing.txt",
        "mkdir ~/.cache",
    ] {
        service
            .sandboxed(workspace.request(command))
            .await
            .unwrap()
            .assert_failed();
    }

    assert_eq!(
        fs::read_dir(&workspace.home).unwrap().count(),
        1,
        "nothing new in HOME"
    );
    assert_eq!(
        fs::read_to_string(workspace.home.join("existing.txt")).unwrap(),
        "original\n"
    );
}

#[tokio::test]
async fn files_cannot_be_moved_or_linked_from_the_checkout_into_the_writable_directory() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    // A hard or symbolic link would make a checkout file writable through its writable name.
    for command in [
        "ln existing.txt \"$TMPDIR/linked-$$.txt\"",
        "mv existing.txt \"$TMPDIR/moved-$$.txt\"",
        "ln -s \"$PWD/existing.txt\" \"$TMPDIR/symlink-$$\"; echo changed > \"$TMPDIR/symlink-$$\"",
    ] {
        service
            .sandboxed(workspace.request(command))
            .await
            .unwrap()
            .assert_failed();
        workspace.assert_checkout_untouched();
    }
}

#[tokio::test]
async fn files_cannot_be_moved_from_the_writable_directory_into_the_checkout() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    service
        .sandboxed(
            workspace.request(
                "echo payload > \"$TMPDIR/payload-$$\"; mv \"$TMPDIR/payload-$$\" new.txt",
            ),
        )
        .await
        .unwrap()
        .assert_failed();

    workspace.assert_checkout_untouched();
}

#[tokio::test]
async fn writes_work_in_the_private_directory_that_temp_variables_point_to() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    let outcome = service
        .sandboxed(workspace.request(
            r#"set -e
for variable in "$TMPDIR" "$TMP" "$TEMP" "$TEMPDIR"; do
    test "$variable" = /tmp/read-only-shell
done
dir=$(mktemp -d)
case "$dir" in /tmp/read-only-shell/*) ;; *) exit 9 ;; esac
echo scratch > "$dir/note"; cat "$dir/note"; : > "$dir/note"
mkdir "$dir/sub"; mv "$dir/note" "$dir/sub/note"; rm -r "$dir"
file=$(mktemp); rm "$file"
cat <<EOF
heredoc
EOF
"#,
        ))
        .await
        .unwrap();

    outcome.assert_succeeded();
    assert_eq!(outcome.stdout, "scratch\nheredoc\n");
}

#[tokio::test]
async fn python_tempfile_uses_the_private_directory() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    let outcome = service
        .sandboxed(workspace.request(python(
            r#"import tempfile; print(tempfile.NamedTemporaryFile().name)"#,
        )))
        .await
        .unwrap();

    outcome.assert_succeeded();
    assert!(
        outcome.stdout.starts_with("/tmp/read-only-shell/"),
        "{outcome:#?}"
    );
}

#[tokio::test]
async fn a_request_cannot_point_the_temp_variables_or_the_writable_directory_elsewhere() {
    let service = Service::start().await;
    let workspace = Workspace::new();
    let mut request = workspace.request("printf %s \"$TMPDIR\"; echo denied > \"$PWD/new.txt\"");
    request
        .envs
        .insert("TMPDIR".into(), workspace.checkout.display().to_string());
    request.envs.insert(
        "SWIFTIDE_DOCKER_SERVICE_SANDBOX_DIR".into(),
        workspace.checkout.display().to_string(),
    );

    let outcome = service.sandboxed(request).await.unwrap();

    outcome.assert_failed();
    assert_eq!(outcome.stdout, "/tmp/read-only-shell");
    workspace.assert_checkout_untouched();
}

#[tokio::test]
async fn the_rest_of_tmp_is_readable_but_not_writable() {
    let service = Service::start().await;
    let workspace = Workspace::new();
    let tmp = tempfile::tempdir().unwrap();
    let existing = tmp.path().join("existing");
    fs::write(&existing, "original\n").unwrap();
    let tmp_display = tmp.path().display();
    let existing_display = existing.display();

    let read = service
        .sandboxed(workspace.request(format!("cat {existing_display}; ls /tmp > /dev/null")))
        .await
        .unwrap();
    read.assert_succeeded();
    assert_eq!(read.stdout, "original\n");

    let top_level = format!("/tmp/planted-by-sandbox-{}", std::process::id());
    for command in [
        format!("echo new > {tmp_display}/new"),
        format!("echo changed > {existing_display}"),
        format!(": > {existing_display}"),
        format!("rm {existing_display}"),
        format!("mv {existing_display} {tmp_display}/moved"),
        format!("mkdir {tmp_display}/directory"),
        format!("touch {top_level}"),
    ] {
        service
            .sandboxed(workspace.request(command.as_str()))
            .await
            .unwrap()
            .assert_failed();
    }

    assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1, "nothing new");
    assert_eq!(fs::read_to_string(&existing).unwrap(), "original\n");
    assert!(!Path::new(&top_level).exists());
}

#[tokio::test]
async fn a_sandboxed_process_cannot_rewrite_the_script_of_a_running_normal_command() {
    let service = Service::start().await;
    let workspace = Workspace::new();
    let tmp = tempfile::tempdir().unwrap();
    let script_path = tmp.path().join("script-path");
    let script_path_display = script_path.display();

    let (normal, sandboxed) = tokio::join!(
        service.normal(workspace.request(format!(
            "#!/bin/sh\necho \"$0\" > {script_path_display}\nsleep 1.5\necho the-original-script\n"
        ))),
        service.sandboxed(workspace.request(format!(
            r#"until [ -s {script_path_display} ]; do sleep 0.02; done
script=$(cat {script_path_display})
case "$script" in /tmp/swiftide-script-*) echo in-script-directory ;; esac
echo "echo hijacked" > "$script" || echo cannot-rewrite
: > "$script" || echo cannot-truncate
touch "$(dirname "$script")/planted" || echo cannot-plant
mv "$script" "$script.moved" || echo cannot-move
"#
        ))),
    );

    let sandboxed = sandboxed.unwrap();
    assert_eq!(
        sandboxed.stdout,
        "in-script-directory\ncannot-rewrite\ncannot-truncate\ncannot-plant\ncannot-move\n",
        "{sandboxed:#?}"
    );
    let normal = normal.unwrap();
    normal.assert_succeeded();
    assert_eq!(normal.stdout, "the-original-script\n");
}

#[track_caller]
fn assert_directory_rejected(status: &Status, reason: &str) {
    assert_eq!(status.code(), Code::FailedPrecondition, "{status}");
    assert!(status.message().contains("did not run"), "{status}");
    assert!(status.message().contains(reason), "{status}");
}

#[tokio::test]
async fn a_symlink_planted_at_the_writable_directory_fails_closed_and_writes_nothing_through_it() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = Workspace::new();
    let planted = tmp.path().join("read-only-shell");
    std::os::unix::fs::symlink(&workspace.checkout, &planted).unwrap();
    let service = Service::with_sandbox_directory(&planted).await;

    let status = service
        .sandboxed(workspace.request("echo new > \"$TMPDIR/new\""))
        .await
        .unwrap_err();

    assert_directory_rejected(&status, "symbolic link");
    workspace.assert_checkout_untouched();
}

#[tokio::test]
async fn a_dangling_symlink_planted_at_the_writable_directory_creates_nothing_at_its_target() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = Workspace::new();
    let planted = tmp.path().join("read-only-shell");
    let target = workspace.checkout.join("created-through-symlink");
    std::os::unix::fs::symlink(&target, &planted).unwrap();
    let service = Service::with_sandbox_directory(&planted).await;

    let status = service
        .sandboxed(workspace.request("touch \"$TMPDIR/new\""))
        .await
        .unwrap_err();

    assert_directory_rejected(&status, "symbolic link");
    assert!(!target.exists());
    workspace.assert_checkout_untouched();
}

#[tokio::test]
async fn a_file_planted_at_the_writable_directory_fails_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = Workspace::new();
    let planted = tmp.path().join("read-only-shell");
    fs::write(&planted, "planted").unwrap();
    let service = Service::with_sandbox_directory(&planted).await;

    let status = service
        .sandboxed(workspace.request("echo should-not-run"))
        .await
        .unwrap_err();

    assert_directory_rejected(&status, "not a directory");
    assert_eq!(fs::read_to_string(&planted).unwrap(), "planted");
}

#[tokio::test]
async fn an_open_writable_directory_fails_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = Workspace::new();
    let planted = tmp.path().join("read-only-shell");
    fs::create_dir(&planted).unwrap();
    fs::set_permissions(&planted, fs::Permissions::from_mode(0o777)).unwrap();
    let service = Service::with_sandbox_directory(&planted).await;

    let status = service
        .sandboxed(workspace.request("echo should-not-run"))
        .await
        .unwrap_err();

    assert_directory_rejected(&status, "mode 0777");
}

#[tokio::test]
async fn a_directory_owned_by_another_user_fails_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = Workspace::new();
    let planted = tmp.path().join("read-only-shell");
    fs::create_dir(&planted).unwrap();
    fs::set_permissions(&planted, fs::Permissions::from_mode(0o700)).unwrap();
    if std::os::unix::fs::chown(&planted, Some(12345), Some(12345)).is_err() {
        eprintln!("skipped: giving a directory to another user needs CAP_CHOWN");
        return;
    }
    let service = Service::with_sandbox_directory(&planted).await;

    let status = service
        .sandboxed(workspace.request("echo should-not-run"))
        .await
        .unwrap_err();

    assert_directory_rejected(&status, "owned by uid 12345");
}

#[tokio::test]
async fn a_sound_directory_left_by_an_earlier_call_is_reused() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = Workspace::new();
    let directory = tmp.path().join("read-only-shell");
    let service = Service::with_sandbox_directory(&directory).await;

    service
        .sandboxed(workspace.request("echo kept > \"$TMPDIR/note\""))
        .await
        .unwrap()
        .assert_succeeded();
    let outcome = service
        .sandboxed(workspace.request("cat \"$TMPDIR/note\""))
        .await
        .unwrap();

    outcome.assert_succeeded();
    assert_eq!(outcome.stdout, "kept\n");
    assert_eq!(
        fs::metadata(&directory).unwrap().permissions().mode() & 0o7777,
        0o700
    );
}

#[tokio::test]
async fn a_shell_that_opens_up_its_directory_makes_later_calls_fail_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let workspace = Workspace::new();
    let directory = tmp.path().join("read-only-shell");
    let service = Service::with_sandbox_directory(&directory).await;

    service
        .sandboxed(workspace.request("chmod 777 \"$TMPDIR\""))
        .await
        .unwrap()
        .assert_succeeded();
    let status = service
        .sandboxed(workspace.request("echo should-not-run"))
        .await
        .unwrap_err();

    assert_directory_rejected(&status, "mode 0777");
}

#[tokio::test]
async fn a_normal_command_does_not_get_the_sandbox_temp_variables() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    let outcome = service
        .normal(workspace.request("printf %s \"$TMPDIR\""))
        .await
        .unwrap();

    assert_ne!(outcome.stdout, "/tmp/read-only-shell");
}

#[tokio::test]
async fn device_nodes_and_redirected_output_stay_writable() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    let outcome = service
        .sandboxed(workspace.request(
            "set -e; echo gone > /dev/null; : > /dev/null; echo gone >> /dev/null; \
             head -c 1 /dev/zero > /dev/null; ! echo x 2> /dev/null > /dev/full; \
             echo to-stdout > /dev/stdout; echo to-stderr > /dev/stderr",
        ))
        .await
        .unwrap();

    outcome.assert_succeeded();
    assert_eq!(outcome.stdout, "to-stdout\n");
    assert_eq!(outcome.stderr, "to-stderr\n");
}

#[tokio::test]
async fn other_device_nodes_are_not_writable() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    let outcome = service
        .sandboxed(workspace.request(python(
            r#"open("/dev/shm/sandbox-test-file", "w").write("x")"#,
        )))
        .await
        .unwrap();

    outcome.assert_failed();
    assert!(outcome.stderr.contains("Permission denied"), "{outcome:#?}");
}

#[tokio::test]
async fn tcp_connect_to_a_listening_local_port_is_denied() {
    let service = Service::start().await;
    let workspace = Workspace::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();

    let connect = format!("echo hello > /dev/tcp/127.0.0.1/{port}");
    service
        .normal(workspace.request(connect.as_str()))
        .await
        .unwrap()
        .assert_succeeded();
    assert!(
        listener.accept().is_ok(),
        "the same connect works without the sandbox"
    );

    let outcome = service.sandboxed(workspace.request(connect)).await.unwrap();

    outcome.assert_failed();
    assert!(outcome.stderr.contains("Permission denied"), "{outcome:#?}");
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        ErrorKind::WouldBlock,
        "the listener must not see a connection"
    );
}

#[tokio::test]
async fn the_services_own_grpc_port_is_unreachable() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    let outcome = service
        .sandboxed(workspace.request(format!(
            "echo hello > /dev/tcp/{}/{}",
            service.ip,
            Service::PORT
        )))
        .await
        .unwrap();

    outcome.assert_failed();
    assert!(outcome.stderr.contains("Permission denied"), "{outcome:#?}");
}

#[tokio::test]
async fn tcp_over_ipv6_is_denied() {
    let Ok(listener) = TcpListener::bind(("::1", 0)) else {
        eprintln!("skipped: this host has no IPv6 loopback");
        return;
    };
    let port = listener.local_addr().unwrap().port();
    let service = Service::start().await;
    let workspace = Workspace::new();

    service
        .sandboxed(workspace.request(format!("echo hello > /dev/tcp/::1/{port}")))
        .await
        .unwrap()
        .assert_failed();
}

#[tokio::test]
async fn tcp_bind_is_denied() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    let outcome = service
        .sandboxed(workspace.request(python(
            r#"import socket; socket.socket().bind(("127.0.0.1", 0))"#,
        )))
        .await
        .unwrap();

    outcome.assert_failed();
    assert!(outcome.stderr.contains("Permission denied"), "{outcome:#?}");
}

#[tokio::test]
async fn a_normal_call_still_writes_after_a_sandboxed_call() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    service
        .sandboxed(workspace.request("echo denied > sandboxed.txt"))
        .await
        .unwrap()
        .assert_failed();
    service
        .normal(workspace.request("echo allowed > normal.txt"))
        .await
        .unwrap()
        .assert_succeeded();
    service
        .sandboxed(workspace.request("echo denied > sandboxed-again.txt"))
        .await
        .unwrap()
        .assert_failed();

    assert_eq!(workspace.checkout_entries(), ["existing.txt", "normal.txt"]);
}

#[tokio::test]
async fn a_normal_call_writes_while_a_sandboxed_call_is_running() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    let (sandboxed, normal) = tokio::join!(
        service.sandboxed(workspace.request("sleep 1; echo denied > sandboxed.txt")),
        service.normal(workspace.request("sleep 0.2; echo allowed > normal.txt")),
    );

    sandboxed.unwrap().assert_failed();
    normal.unwrap().assert_succeeded();
    assert_eq!(workspace.checkout_entries(), ["existing.txt", "normal.txt"]);
}

#[tokio::test]
async fn child_processes_inherit_the_sandbox() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    for command in [
        "bash -c 'echo denied > new.txt'",
        "sh -c 'sh -c \"echo denied > new.txt\"'",
        "setsid --wait sh -c 'echo denied > new.txt'",
        "nohup sh -c 'echo denied > new.txt' > /dev/null",
        &python(r#"import subprocess; subprocess.run(["touch", "new.txt"], check=True)"#),
    ] {
        service
            .sandboxed(workspace.request(command))
            .await
            .unwrap()
            .assert_failed();
        workspace.assert_checkout_untouched();
    }
}

#[tokio::test]
async fn a_child_cannot_reach_tcp_either() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    service
        .sandboxed(workspace.request(format!(
            "bash -c 'echo hello > /dev/tcp/{}/{}'",
            service.ip,
            Service::PORT
        )))
        .await
        .unwrap()
        .assert_failed();
}

#[tokio::test]
async fn login_profiles_run_inside_the_sandbox() {
    let service = Service::start().await;
    let workspace = Workspace::new();
    fs::write(
        workspace.home.join(".bash_profile"),
        "export PROFILE_RAN=yes\necho denied > \"$HOME/written-by-profile\"\n",
    )
    .unwrap();

    let outcome = service
        .sandboxed(workspace.request("printf %s \"$PROFILE_RAN\""))
        .await
        .unwrap();

    outcome.assert_succeeded();
    assert_eq!(outcome.stdout, "yes", "the profile should have run");
    assert!(!workspace.home.join("written-by-profile").exists());
}

#[tokio::test]
async fn background_commands_report_as_on_the_normal_path() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    let normal = service
        .normal(workspace.request("sleep 0.01 &"))
        .await
        .unwrap();
    let sandboxed = service
        .sandboxed(workspace.request("sleep 0.01 &"))
        .await
        .unwrap();

    assert_eq!(sandboxed.stdout, "Background command started");
    assert_eq!(sandboxed.stdout, normal.stdout);
    assert_eq!(sandboxed.stderr, normal.stderr);
    assert_eq!(sandboxed.result, normal.result);
}

#[tokio::test]
async fn background_commands_run_inside_the_sandbox() {
    let tmp = tempfile::tempdir().unwrap();
    let directory = tmp.path().join("read-only-shell");
    let service = Service::with_sandbox_directory(&directory).await;
    let workspace = Workspace::new();
    let done = directory.join("done");

    service
        .sandboxed(workspace.request("(echo denied > new.txt; echo finished > \"$TMPDIR/done\") &"))
        .await
        .unwrap()
        .assert_succeeded();

    eventually("the background command wrote to its directory", || {
        done.exists()
    })
    .await;
    workspace.assert_checkout_untouched();
}

#[tokio::test]
async fn a_background_command_that_outlives_its_call_still_cannot_write_to_tmp() {
    let tmp = tempfile::tempdir().unwrap();
    let directory = tmp.path().join("read-only-shell");
    let service = Service::with_sandbox_directory(&directory).await;
    let workspace = Workspace::new();
    let shared = tempfile::tempdir().unwrap();
    let planted = shared.path().join("planted");
    let done = directory.join("done");

    service
        .sandboxed(workspace.request(format!(
            "(sleep 0.3; echo planted > {}; echo finished > \"$TMPDIR/done\") &",
            planted.display()
        )))
        .await
        .unwrap()
        .assert_succeeded();

    eventually("the background command finished", || done.exists()).await;
    assert!(!planted.exists());
}

#[tokio::test]
async fn cwd_env_and_exit_codes_behave_as_on_the_normal_path() {
    let service = Service::start().await;
    let workspace = Workspace::new();
    let mut request = workspace.request("pwd; printf '%s/%s\\n' \"$KEPT\" \"$DROPPED\"; exit 7");
    request.envs.insert("KEPT".into(), "kept".into());
    request.envs.insert("DROPPED".into(), "dropped".into());
    request.env_remove = vec!["DROPPED".into()];

    let normal = service.normal(request.clone()).await.unwrap();
    let sandboxed = service.sandboxed(request).await.unwrap();

    assert_eq!(sandboxed.result, Event::ExitCode(7));
    assert!(sandboxed.stdout.contains("/checkout\n"), "{sandboxed:#?}");
    assert_eq!(sandboxed.stdout, normal.stdout);
}

#[tokio::test]
async fn env_clear_applies_inside_the_sandbox() {
    let service = Service::start().await;
    let workspace = Workspace::new();
    let mut request = workspace.request("printf %s \"${HOME:-cleared}/${ONLY:-}\"");
    request.env_clear = true;
    request.envs = HashMap::from([("ONLY".into(), "this".into())]);

    let outcome = service.sandboxed(request).await.unwrap();

    outcome.assert_succeeded();
    assert!(outcome.stdout.ends_with("/this"), "{outcome:#?}");
}

#[tokio::test]
async fn shebang_commands_run_through_their_interpreter_inside_the_sandbox() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    let outcome = service
        .sandboxed(workspace.request(
            "#!/usr/bin/env python3\nimport os\nprint('py-ok')\nos.open('new.txt', os.O_CREAT)",
        ))
        .await
        .unwrap();

    outcome.assert_failed();
    assert_eq!(outcome.stdout, "py-ok\n");
    workspace.assert_checkout_untouched();
}

#[tokio::test]
async fn timeouts_return_partial_output_and_kill_the_command() {
    let service = Service::start().await;
    let workspace = Workspace::new();
    let mut request = workspace.request("printf before-timeout; sleep 60; echo denied > new.txt");
    request.timeout_ms = Some(500);

    let outcome = service.sandboxed(request).await.unwrap();

    assert_eq!(outcome.result, Event::TimedOutAfterMs(500));
    assert_eq!(outcome.stdout, "before-timeout");
}

#[tokio::test]
async fn a_missing_working_directory_fails_as_on_the_normal_path() {
    let service = Service::start().await;
    let workspace = Workspace::new();
    let mut request = workspace.request("echo should-not-run");
    request.cwd = Some("/no/such/directory".into());

    let normal = service.normal(request.clone()).await.unwrap_err();
    let sandboxed = service.sandboxed(request).await.unwrap_err();

    assert_eq!(sandboxed.code(), normal.code());
    assert_eq!(sandboxed.message(), normal.message());
}

#[tokio::test]
async fn a_missing_interpreter_fails_with_an_error_instead_of_an_exit_code() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    let status = service
        .sandboxed(workspace.request("#!/no/such/interpreter\necho should-not-run"))
        .await
        .unwrap_err();

    assert_eq!(status.code(), Code::Internal);
    assert!(
        status.message().contains("/no/such/interpreter"),
        "{status}"
    );
}

#[tokio::test]
async fn the_shell_runs_without_new_privileges_or_capabilities() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    let outcome = service
        .sandboxed(workspace.request("cat /proc/self/status"))
        .await
        .unwrap();

    outcome.assert_succeeded();
    let field = |name: &str| {
        outcome
            .stdout
            .lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
            .unwrap_or_else(|| panic!("{name} in {}", outcome.stdout))
            .trim()
    };
    assert_eq!(field("NoNewPrivs"), "1");
    for set in ["CapInh", "CapPrm", "CapEff", "CapAmb"] {
        assert_eq!(field(set), "0000000000000000", "{set}");
    }
    // Only a process with CAP_SETPCAP can clear its bounding set. For anyone else it stays, but
    // it grants nothing: only root regains capabilities from it on exec.
    if unsafe { libc::geteuid() } == 0 {
        assert_eq!(field("CapBnd"), "0000000000000000", "CapBnd");
    }
}

#[tokio::test]
async fn the_shell_inherits_only_stdio_from_the_service() {
    let scratch = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let leaked_path = scratch.path().join("leaked");
    let mut leaked = fs::File::create(&leaked_path).unwrap();
    leaked.write_all(b"original").unwrap();
    let leaked_fd = leaked.as_raw_fd();

    let mut command = Command::new(env!("CARGO_BIN_EXE_swiftide-docker-service"));
    // SAFETY: fcntl is async-signal-safe.
    unsafe {
        command.pre_exec(move || {
            // A writable descriptor that whoever started the service forgot to close. Landlock
            // only checks at open, so the shell could write through it if it were inherited.
            if libc::fcntl(leaked_fd, libc::F_SETFD, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let service = Service::start_with(command).await;
    let workspace = Workspace::new();

    service
        .normal(workspace.request(format!("echo -n written >&{leaked_fd}")))
        .await
        .unwrap()
        .assert_succeeded();
    assert_eq!(
        fs::read_to_string(&leaked_path).unwrap(),
        "originalwritten",
        "the descriptor really is inherited by the service"
    );

    service
        .sandboxed(workspace.request(format!("echo -n denied >&{leaked_fd}")))
        .await
        .unwrap()
        .assert_failed();
    assert_eq!(fs::read_to_string(&leaked_path).unwrap(), "originalwritten");

    let descriptors = service
        .sandboxed(workspace.request("ls /proc/self/fd"))
        .await
        .unwrap();
    // 0, 1 and 2, plus the descriptor `ls` itself opens to read the directory.
    assert_eq!(descriptors.stdout.lines().count(), 4, "{descriptors:#?}");
}

/// Starts the service with `landlock_create_ruleset` answering ENOSYS, which is what a kernel
/// without Landlock answers: real kernel behaviour through a seccomp filter, with no test seam in
/// the service.
fn service_on_a_kernel_without_landlock() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_swiftide-docker-service"));
    // SAFETY: only async-signal-safe prctl calls on a filter built on the stack.
    unsafe {
        command.pre_exec(|| {
            const LOAD_SYSCALL_NUMBER: u16 = 0x20;
            const JUMP_IF_EQUAL: u16 = 0x15;
            const RETURN: u16 = 0x06;
            let mut filter = [
                libc::sock_filter {
                    code: LOAD_SYSCALL_NUMBER,
                    jt: 0,
                    jf: 0,
                    k: 0,
                },
                libc::sock_filter {
                    code: JUMP_IF_EQUAL,
                    jt: 0,
                    jf: 1,
                    k: libc::SYS_landlock_create_ruleset as u32,
                },
                libc::sock_filter {
                    code: RETURN,
                    jt: 0,
                    jf: 0,
                    k: libc::SECCOMP_RET_ERRNO | libc::ENOSYS as u32,
                },
                libc::sock_filter {
                    code: RETURN,
                    jt: 0,
                    jf: 0,
                    k: libc::SECCOMP_RET_ALLOW,
                },
            ];
            let program = libc::sock_fprog {
                len: filter.len() as u16,
                filter: filter.as_mut_ptr(),
            };
            if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                || libc::prctl(
                    libc::PR_SET_SECCOMP,
                    libc::SECCOMP_MODE_FILTER,
                    &raw const program,
                ) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    command
}

#[track_caller]
fn assert_names_missing_landlock(status: &Status) {
    assert_eq!(status.code(), Code::FailedPrecondition, "{status}");
    assert!(status.message().contains("Landlock"), "{status}");
    assert!(status.message().contains("did not run"), "{status}");
}

#[tokio::test]
async fn without_landlock_the_command_does_not_run_and_the_error_says_why() {
    let service = Service::start_with(service_on_a_kernel_without_landlock()).await;
    let workspace = Workspace::new();
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("ran");

    let status = service
        .sandboxed(workspace.request(format!("touch {}", marker.display())))
        .await
        .unwrap_err();

    assert_names_missing_landlock(&status);
    assert!(!marker.exists(), "the command must not have run");
}

#[tokio::test]
async fn without_landlock_a_background_command_does_not_run_and_the_error_says_why() {
    let service = Service::start_with(service_on_a_kernel_without_landlock()).await;
    let workspace = Workspace::new();
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("ran");

    let status = service
        .sandboxed(workspace.request(format!("touch {} &", marker.display())))
        .await
        .unwrap_err();

    assert_names_missing_landlock(&status);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!marker.exists(), "the command must not have run");
}

#[tokio::test]
async fn without_landlock_normal_calls_keep_working() {
    let service = Service::start_with(service_on_a_kernel_without_landlock()).await;
    let workspace = Workspace::new();

    service
        .normal(workspace.request("echo allowed > normal.txt"))
        .await
        .unwrap()
        .assert_succeeded();

    assert!(Path::new(&workspace.checkout).join("normal.txt").exists());
}

/// Connects through `sendmmsg`, which has no Python binding.
fn fast_open_with_sendmmsg(port: u16) -> String {
    format!(
        r#"
import ctypes, socket, struct
class IoVec(ctypes.Structure):
    _fields_ = [("base", ctypes.c_char_p), ("len", ctypes.c_size_t)]
class Message(ctypes.Structure):
    _fields_ = [("name", ctypes.c_char_p), ("namelen", ctypes.c_uint32),
                ("iov", ctypes.POINTER(IoVec)), ("iovlen", ctypes.c_size_t),
                ("control", ctypes.c_void_p), ("controllen", ctypes.c_size_t),
                ("flags", ctypes.c_int), ("sent", ctypes.c_uint)]
address = struct.pack("=HH4s8x", socket.AF_INET, socket.htons({port}), socket.inet_aton("127.0.0.1"))
iov = IoVec(b"x", 1)
message = Message(address, len(address), ctypes.pointer(iov), 1, None, 0, 0, 0)
libc = ctypes.CDLL(None, use_errno=True)
s = socket.socket()
if libc.sendmmsg(s.fileno(), ctypes.byref(message), 1, socket.MSG_FASTOPEN) != 1:
    raise OSError(ctypes.get_errno(), "sendmmsg")
"#
    )
}

#[tokio::test]
async fn tcp_cannot_be_reached_around_connect() {
    let service = Service::start().await;
    let workspace = Workspace::new();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    listener.set_nonblocking(true).unwrap();
    let port = listener.local_addr().unwrap().port();
    let address = format!(r#"("127.0.0.1", {port})"#);

    let mut reached = Vec::new();
    for (way, script) in [
        (
            "fast open with sendto",
            format!(
                r#"import socket; socket.socket().sendto(b"x", socket.MSG_FASTOPEN, {address})"#
            ),
        ),
        (
            "fast open with sendmsg",
            format!(
                r#"import socket; socket.socket().sendmsg([b"x"], [], socket.MSG_FASTOPEN, {address})"#
            ),
        ),
        ("fast open with sendmmsg", fast_open_with_sendmmsg(port)),
        (
            "multipath TCP",
            format!(
                r#"import socket; socket.socket(socket.AF_INET, socket.SOCK_STREAM, socket.IPPROTO_MPTCP).connect({address})"#
            ),
        ),
        (
            "SMC",
            format!(
                r#"import socket; socket.socket(43, socket.SOCK_STREAM, 0).connect({address})"#
            ),
        ),
        (
            "connect after asking for fast open",
            format!(
                r#"import socket; s = socket.socket(); s.setsockopt(socket.IPPROTO_TCP, socket.TCP_FASTOPEN_CONNECT, 1); s.connect({address})"#
            ),
        ),
    ] {
        let outcome = service
            .sandboxed(workspace.request(python(&script)))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let accepted = listener.accept().is_ok();
        if accepted || !matches!(outcome.result, Event::ExitCode(code) if code != 0) {
            reached.push((way, accepted, outcome));
        }
    }

    assert!(reached.is_empty(), "TCP was reached through {reached:#?}");
}

#[tokio::test]
async fn io_uring_is_denied() {
    let service = Service::start().await;
    let workspace = Workspace::new();

    let outcome = service
        .sandboxed(workspace.request(python(
            r#"
import ctypes, sys
libc = ctypes.CDLL(None, use_errno=True)
parameters = ctypes.create_string_buffer(120)
fd = libc.syscall(425, 4, parameters)
sys.exit("ring created" if fd >= 0 else ctypes.get_errno() == 13 and "denied" or "other")
"#,
        )))
        .await
        .unwrap();

    outcome.assert_failed();
    assert_eq!(outcome.stderr.trim(), "denied", "{outcome:#?}");
}

#[tokio::test]
async fn ordinary_sockets_stay_available() {
    let service = Service::start().await;
    let workspace = Workspace::new();
    let receiver = std::net::UdpSocket::bind(("127.0.0.1", 0)).unwrap();
    let port = receiver.local_addr().unwrap().port();
    receiver
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let outcome = service
        .sandboxed(workspace.request(python(&format!(
            r#"
import socket
socket.socket(socket.AF_UNIX).close()
socket.socket(socket.AF_NETLINK, socket.SOCK_RAW, 0).close()
socket.socket(socket.AF_INET, socket.SOCK_DGRAM).sendto(b"x", ("127.0.0.1", {port}))
"#
        ))))
        .await
        .unwrap();

    outcome.assert_succeeded();
    receiver.recv(&mut [0; 1]).unwrap();
}
