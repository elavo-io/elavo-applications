//! `elavo-applications`: the Applications folder services.
//!
//! Without arguments it is the account's service (`elavo-applications.service`,
//! a user unit). With `--system` it is one pass of the root service
//! (`elavo-applications-system.service`), started at boot and by a path unit
//! when `/var/Applications` or the system Flatpaks change.

mod prompt;
mod system;
mod user;

fn main() -> std::process::ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--system") => system::run(),
        None => {
            let code = user::run();
            if code == gtk::glib::ExitCode::SUCCESS {
                std::process::ExitCode::SUCCESS
            } else {
                std::process::ExitCode::FAILURE
            }
        }
        Some(other) => {
            eprintln!("elavo-applications: unknown argument {other}");
            std::process::ExitCode::FAILURE
        }
    }
}
