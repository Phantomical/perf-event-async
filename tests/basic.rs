use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::io::FromRawFd;

use assert_matches::assert_matches;
use nix::libc::_exit;
use nix::sys::wait::{waitpid, WaitStatus};
use nix::unistd::ForkResult;
use perf_event::events::Software;
use perf_event::samples::RecordType;
use perf_event::Builder;
use perf_event_async::AsyncSamplerExt;

#[tokio::test(flavor = "current_thread")]
async fn test_basic_fork() {
  let builder = Builder::new()
    .kind(Software::DUMMY)
    .enable_on_exec(true)
    .task(true);

  let (mut remote, mut local) = nix::unistd::pipe()
    .map(|(rx, tx)| unsafe { (File::from_raw_fd(rx), File::from_raw_fd(tx)) })
    .expect("Failed to create a pipe");

  let pid = match unsafe { nix::unistd::fork() }.expect("Failed to fork") {
    ForkResult::Parent { child } => child,
    ForkResult::Child => {
      // It is unsafe to do anything that requires allocation here
      let mut buf = [0];
      match remote.read_exact(&mut buf) {
        Ok(_) => unsafe { _exit(0) },
        Err(e) => {
          eprintln!("error while reading from pipe: {}", e);
          unsafe { _exit(127) }
        }
      }
    }
  };

  let mut sampler = builder
    .observe_pid(pid.as_raw())
    .build_sampler(8192)
    .expect("Failed to construct sampler");

  local
    .write_all(b"a")
    .expect("Failed to write to local pipe");

  while let Some(evt) = sampler.next_async().await {
    assert_eq!(evt.ty, RecordType::EXIT);
  }

  let status = waitpid(pid, None).expect("Failed to wait for child process");

  assert_matches!(status, WaitStatus::Exited(_, 0));
}
