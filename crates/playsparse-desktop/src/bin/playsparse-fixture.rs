//! Generated native compatibility workload. No assets, shells or broker processes.
use std::{env, process::Command, thread, time::Duration};
#[allow(clippy::zombie_processes)] // Deliberately models launchers that exit before their children.
fn main() {
    let args: Vec<_> = env::args().collect();
    match args.get(1).map(String::as_str).unwrap_or("plain") {
        "crash" => std::process::exit(42),
        "launcher" | "multiple" | "child-first" => {
            thread::sleep(Duration::from_millis(200));
            let count = if args[1] == "multiple" { 3 } else { 1 };
            let mode = if args[1] == "child-first" {
                "short"
            } else {
                "plain"
            };
            for _ in 0..count {
                let _child = Command::new(env::current_exe().unwrap())
                    .arg(mode)
                    .spawn()
                    .unwrap();
            }
            if args[1] == "child-first" {
                thread::sleep(Duration::from_secs(4));
            }
        }
        "case" => {
            assert_eq!(
                std::fs::read("Case/Data.txt").unwrap(),
                b"case-sensitive fixture"
            );
            assert_eq!(
                std::fs::read("Case/data.txt").unwrap(),
                b"different case fixture"
            );
        }
        "save" => {
            std::fs::write("fixture-save.txt", b"isolated save").unwrap();
            thread::sleep(Duration::from_secs(5));
        }
        "argv" => {
            assert_eq!(args.get(2).unwrap(), "$(must-not-expand); & | > <");
        }
        "short" => thread::sleep(Duration::from_millis(500)),
        "plain" => thread::sleep(Duration::from_secs(5)),
        other => panic!("Unknown synthetic mode {other}"),
    }
}
