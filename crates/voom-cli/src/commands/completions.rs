use anyhow::Result;
use clap::CommandFactory;
use clap_complete::generate;

use crate::cli::{Cli, CompletionsArgs};

pub fn run(args: CompletionsArgs) -> Result<()> {
    let mut cmd = Cli::command();
    generate(args.shell, &mut cmd, "voom", &mut std::io::stdout());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_bash_completions_succeeds() {
        let args = CompletionsArgs {
            shell: clap_complete::Shell::Bash,
        };
        let mut buf = Vec::new();
        let mut cmd = Cli::command();
        generate(args.shell, &mut cmd, "voom", &mut buf);
        let output = String::from_utf8(buf).unwrap();
        assert!(
            output.contains("voom"),
            "completions should reference 'voom'"
        );
    }

    #[test]
    fn test_generate_zsh_completions_succeeds() {
        let args = CompletionsArgs {
            shell: clap_complete::Shell::Zsh,
        };
        let mut buf = Vec::new();
        let mut cmd = Cli::command();
        generate(args.shell, &mut cmd, "voom", &mut buf);
        let output = String::from_utf8(buf).unwrap();
        assert!(!output.is_empty());
    }

    #[test]
    fn test_generate_fish_completions_succeeds() {
        let args = CompletionsArgs {
            shell: clap_complete::Shell::Fish,
        };
        let mut buf = Vec::new();
        let mut cmd = Cli::command();
        generate(args.shell, &mut cmd, "voom", &mut buf);
        let output = String::from_utf8(buf).unwrap();
        assert!(!output.is_empty());
    }
}
