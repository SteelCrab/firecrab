//! install.sh와 같은 접두를 쓰는 진행 메시지 출력.
//!
//! Task 1에서는 `fail`만 [`super::Error::report`]를 통해 쓰인다. 나머지는
//! 이후 태스크(설치/삭제/systemd 제어 구현)에서 소비된다.
#![allow(dead_code)]

/// `==> msg` — 완료된 단계 (stdout).
pub fn log(msg: &str) {
    println!("\x1b[1;32m==>\x1b[0m {msg}");
}

/// `--  msg` — 시작하는 작업 (stdout).
pub fn step(msg: &str) {
    println!("\x1b[1;36m--\x1b[0m  {msg}");
}

/// `!!  msg` — 계속 진행하는 경고 (stderr).
pub fn warn(msg: &str) {
    eprintln!("\x1b[1;33m!!\x1b[0m  {msg}");
}

/// `xx  msg` — 실패 (stderr). 종료는 호출자가 결정한다.
pub fn fail(msg: &str) {
    eprintln!("\x1b[1;31mxx\x1b[0m  {msg}");
}
