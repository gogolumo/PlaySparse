use playsparse_desktop::process::{LaunchTree, Lifecycle};
use std::{
    process::Command,
    thread,
    time::{Duration, Instant},
};
fn launch(mode: &str) -> LaunchTree {
    let mut command = Command::new(env!("CARGO_BIN_EXE_playsparse-fixture"));
    command.arg(mode);
    if mode == "argv" {
        command.arg("$(must-not-expand); & | > <");
    }
    LaunchTree::prepare(&mut command);
    LaunchTree::attach(command.spawn().unwrap(), false).unwrap()
}
fn eventually(
    tree: &mut LaunchTree,
    predicate: impl Fn(&playsparse_desktop::process::Observation) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(12);
    loop {
        let observation = tree.observe().unwrap();
        if predicate(&observation) {
            return;
        }
        assert!(Instant::now() < deadline, "{observation:?}");
        thread::sleep(Duration::from_millis(50));
    }
}
#[test]
fn direct_owned_process_and_literal_shell_metacharacters() {
    let mut tree = launch("plain");
    assert_eq!(tree.observe().unwrap().lifecycle, Lifecycle::Running);
    assert!(tree.stop_root().unwrap().active_processes.is_empty());
    let mut tree = launch("argv");
    eventually(&mut tree, |o| o.active_processes.is_empty());
}
#[test]
fn launcher_exits_first_descendants_remain_active() {
    let mut tree = launch("launcher");
    eventually(&mut tree, |o| {
        o.lifecycle == Lifecycle::LauncherExitedButGameRunning
    });
    assert!(!tree.observe().unwrap().safe_to_unmount);
    eventually(&mut tree, |o| o.active_processes.is_empty());
}
#[test]
fn multiple_descendants_are_observed_after_reparenting() {
    let mut tree = launch("multiple");
    eventually(&mut tree, |o| o.active_processes.len() >= 3);
    eventually(&mut tree, |o| o.active_processes.is_empty());
}
#[test]
fn child_exits_before_owned_root() {
    let mut tree = launch("child-first");
    eventually(&mut tree, |o| o.active_processes.len() >= 2);
    eventually(&mut tree, |o| {
        o.lifecycle == Lifecycle::Running && o.active_processes.len() == 1
    });
    eventually(&mut tree, |o| o.active_processes.is_empty());
}
