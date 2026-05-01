use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "deepseek")]
#[command(about = "Standalone DeepSeek terminal CLI")]
pub struct Args {
    #[command(subcommand)]
    pub command: Option<Command>,

    #[arg(short = 'p', long)]
    pub prompt: Option<String>,

    #[arg(long)]
    pub no_session: bool,

    #[arg(long)]
    pub model: Option<String>,

    #[arg(long)]
    pub temperature: Option<f32>,

    #[arg(long)]
    pub stream: bool,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Agent {
        #[arg(required = true, trailing_var_arg = true)]
        task: Vec<String>,

        #[arg(long, default_value = ".")]
        root: String,

        #[arg(long, default_value_t = 8)]
        max_steps: usize,
    },
    Login,
    Debug {
        mode: Option<String>,

        #[arg(long)]
        json: bool,
    },
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum SessionCommand {
    Start { name: Option<String> },
    Status,
    End,
}
