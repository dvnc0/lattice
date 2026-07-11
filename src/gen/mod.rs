//! `lattice generate` — generate a Lattice config from an OpenAPI 3.0.x spec file.

pub mod emit;
pub mod openapi;
pub mod render;

use std::path::{Path, PathBuf};

use crate::config::ExposeMode;

/// Output format for the generated config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum GenFormat {
    Yaml,
    Json,
}

/// Expose-mode override passed from the CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum GenExpose {
    Tools,
    Dispatcher,
}

/// Arguments for the `generate` subcommand.
pub struct GenerateArgs {
    pub spec: PathBuf,
    pub output: Option<PathBuf>,
    pub format: GenFormat,
    pub expose: Option<GenExpose>,
}

/// Entry point for `lattice generate`.
pub fn run(args: &GenerateArgs) -> anyhow::Result<()> {
    let (input, parse_warnings) = openapi::parse(&args.spec).map_err(|e| anyhow::anyhow!("{e}"))?;

    for w in &parse_warnings {
        eprintln!("warning: {w}");
    }

    let expose_override = args.expose.map(|e| match e {
        GenExpose::Tools => ExposeMode::Tools,
        GenExpose::Dispatcher => ExposeMode::Dispatcher,
    });

    let (config, emit_warnings) = emit::emit(&input, expose_override);

    for w in &emit_warnings {
        eprintln!("warning: {w}");
    }

    let content = match args.format {
        GenFormat::Yaml => render::to_yaml(&config)?,
        GenFormat::Json => render::to_json(&config)?,
    };

    write_output(args.output.as_deref(), &content)
}

/// Write `content` to a file, or to stdout when `output` is `None`.
pub fn write_output(output: Option<&Path>, content: &str) -> anyhow::Result<()> {
    match output {
        Some(path) => std::fs::write(path, content)
            .map_err(|e| anyhow::anyhow!("failed to write output to {}: {e}", path.display())),
        None => {
            use std::io::Write as _;
            std::io::stdout()
                .lock()
                .write_all(content.as_bytes())
                .map_err(|e| anyhow::anyhow!("failed to write to stdout: {e}"))
        }
    }
}
