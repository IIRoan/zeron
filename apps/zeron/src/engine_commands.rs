//! The subcommands that run the engine's own code ([`zeron_cli::EngineCommand`]),
//! in a binary that links it: `zeron` itself, and `zeron-engine`, the helper
//! an interface without the engine hands them to. Logging is set up by
//! [`zeron_cli::run`] before these run.

use zeron_cli::EngineCommand;

use crate::{auth_cli, engine_config_from_env, ipc_cli, plugin_cli};

pub fn run_engine_command(command: EngineCommand) -> anyhow::Result<()> {
    match command {
        EngineCommand::Headless => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(async {
                let engine = zeron_engine::Engine::new(engine_config_from_env());
                engine.run().await
            })
        }
        EngineCommand::Login => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(auth_cli::login(engine_config_from_env()))
        }
        EngineCommand::Logout => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(auth_cli::logout(engine_config_from_env()))
        }
        EngineCommand::Status => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(auth_cli::status(engine_config_from_env()))
        }
        EngineCommand::Sync => {
            let runtime = tokio::runtime::Runtime::new()?;
            let config = engine_config_from_env();
            runtime.block_on(ipc_cli::sync_cli(config.ipc_port, &config.data_dir))
        }
        EngineCommand::Quiesce => {
            let runtime = tokio::runtime::Runtime::new()?;
            let config = engine_config_from_env();
            runtime.block_on(ipc_cli::quiesce_cli(config.ipc_port, &config.data_dir))
        }
        EngineCommand::Mcp => crate::run_mcp(),
        EngineCommand::Plugin(command) => {
            let runtime = tokio::runtime::Runtime::new()?;
            let config = engine_config_from_env();
            runtime.block_on(plugin_cli::run(command, config.ipc_port, &config.data_dir))
        }
        EngineCommand::Template(command) => {
            let runtime = tokio::runtime::Runtime::new()?;
            let config = engine_config_from_env();
            runtime.block_on(plugin_cli::run_template(
                command,
                config.ipc_port,
                &config.data_dir,
            ))
        }
        EngineCommand::Sandbox(command) => {
            use zeron_engine::sandbox;
            match command {
                zeron_cli::SandboxCommand::Enable {
                    path,
                    config,
                    host_ports,
                    publish_ports,
                    read_only,
                    worktrees,
                } => {
                    let ports = |list: Vec<String>| sandbox::parse_ports(&list.join(","));
                    let (host_ports, publish_ports) = (
                        ports(host_ports).map_err(|e| anyhow::anyhow!("--host-port: {e}"))?,
                        ports(publish_ports).map_err(|e| anyhow::anyhow!("--publish: {e}"))?,
                    );
                    sandbox::enable(
                        sandbox::Sandbox {
                            worktrees: worktrees.unwrap_or_else(|| sandbox::default_worktrees(&path)),
                            root: path,
                            config,
                            host_ports,
                            publish_ports,
                            read_only,
                        },
                        false,
                    )
                }
                .map(|status| {
                    println!("Sandboxed {} in container {}.", status.sandbox.root.display(), status.container);
                    println!("{}", sandbox::LOGIN_NOTE);
                }),
                zeron_cli::SandboxCommand::Disable { path } => sandbox::disable(&path).map(|()| {
                    println!("{} runs on this machine again. Its image and home volume were kept.", path.display());
                }),
                zeron_cli::SandboxCommand::Status => sandbox::statuses().map(|statuses| {
                    if statuses.is_empty() {
                        println!("No sandboxed projects.");
                    }
                    for s in statuses {
                        println!(
                            "{}\n  config: {}\n  container: {} ({}{})\n  worktrees: {}\n  host ports: {:?}, published: {:?}, read-only: {:?}",
                            s.sandbox.root.display(),
                            s.sandbox.config.display(),
                            s.container,
                            s.state,
                            if s.image_built { "" } else { ", image not built" },
                            s.sandbox.worktrees.display(),
                            sandbox::port_ranges(&s.sandbox.host_ports),
                            sandbox::port_ranges(&s.sandbox.publish_ports),
                            s.sandbox.read_only,
                        );
                    }
                }),
            }
            .map_err(|error| anyhow::anyhow!("{error}"))
        }
        EngineCommand::Engine => crate::background::run(),
    }
}
