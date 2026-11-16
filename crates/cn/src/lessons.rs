use std::path::PathBuf;

use conary_core::lessons::{self, Register};

#[derive(clap::Subcommand)]
pub enum LessonsCommand {
    /// Check the register as the gate does: every field present and well
    /// formed, each id once, every `applies` value a declared component. An
    /// open lesson passes; whether a milestone may exit is `exit`.
    Check {
        /// The register, from the repository's root.
        #[arg(long, default_value = lessons::PATH)]
        register: PathBuf,
    },
    /// The lessons part of a milestone's exit check: fails while an open
    /// lesson applies to a component the milestone ships. Passing it doesn't
    /// pass the milestone; its exit check in docs/DESIGN.md's roadmap does.
    Exit {
        /// The milestone, one the register declares (M6, M7).
        milestone: String,
        /// The register, from the repository's root.
        #[arg(long, default_value = lessons::PATH)]
        register: PathBuf,
    },
}

pub fn run(cli: &super::Cli, command: &LessonsCommand) -> super::Result<()> {
    match command {
        LessonsCommand::Check { register } => {
            let r = Register::load(register)?;
            if cli.human {
                println!(
                    "{}: {} lessons, valid",
                    register.display(),
                    r.lessons().len()
                );
            } else {
                println!(
                    "{}",
                    serde_json::json!({"lessons": r.lessons().len(), "milestones": r.milestones()})
                );
            }
        }
        LessonsCommand::Exit {
            milestone,
            register,
        } => {
            let exit = Register::load(register)?.exit(milestone)?;
            if cli.human {
                println!(
                    "{milestone} ships {} components: {}",
                    exit.ships.len(),
                    exit.ships.join(", ")
                );
                if exit.clear() {
                    println!(
                        "  no open lesson applies to them; that's only the lessons part of {milestone}'s exit check"
                    );
                }
                for b in &exit.open {
                    println!(
                        "  {} is open, on {}; owed by {}",
                        b.id,
                        b.applies.join(", "),
                        b.held.join(", ")
                    );
                }
            } else {
                println!("{}", serde_json::to_string(&exit)?);
            }
            if !exit.clear() {
                let ids: Vec<&str> = exit.open.iter().map(|b| b.id.as_str()).collect();
                return Err(format!(
                    "{milestone} can't exit while {} open lessons apply to what it ships: {}",
                    ids.len(),
                    ids.join(", ")
                )
                .into());
            }
        }
    }
    Ok(())
}
