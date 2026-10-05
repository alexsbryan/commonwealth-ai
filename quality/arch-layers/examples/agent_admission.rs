// SPDX-License-Identifier: AGPL-3.0-or-later
#[path = "agent_admission/controller.rs"]
mod controller;
#[path = "agent_admission/fixture.rs"]
mod fixture;
#[path = "agent_admission/oracle.rs"]
mod oracle;

use std::{
    io::{self, BufRead, Write},
    path::Path,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() != 2 || args[0] != "--root" {
        return Err(
            "usage: agent_admission --root <new-run-directory>; JSON actions on stdin".into(),
        );
    }
    let mut host = controller::AdmissionController::new(Path::new(&args[1]))?;
    println!("{}", host.state());
    io::stdout().flush()?;
    for line in io::stdin().lock().lines() {
        let result = host.dispatch(&line?);
        println!(
            "{}",
            serde_json::json!({"result":result,"state":host.state()})
        );
        io::stdout().flush()?;
        if host.accepted_candidate().is_some() || result["status"] == "stopped" {
            break;
        }
    }
    Ok(())
}
