use clap::{Parser, Subcommand};
#[derive(Parser)]
struct Args {
    #[arg(long, env = "ZERON_REMOTE_DATA_DIR")]
    data_dir: Option<std::path::PathBuf>,
    /// Use the separate account-free ZGX desktop gateway and device store.
    #[arg(long)]
    desktop: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Serve on loopback; expose through an HTTPS private-network proxy.
    Serve {
        #[arg(long, default_value = "127.0.0.1:27812")]
        listen: std::net::SocketAddr,
        #[arg(long, env = "ZERON_IPC_PORT", default_value_t = 27811)]
        ipc_port: u16,
    },
    /// Print a one-time pairing key, valid for ten minutes.
    Pair,
    Devices,
    Revoke {
        id: String,
    },
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let root = args.data_dir.unwrap_or_else(|| {
        std::path::PathBuf::from(std::env::var_os("HOME").unwrap_or_default())
            .join(".local/share/zeron-remote")
    });
    let store = zeron_rpc::remote::RemoteStore::open(if args.desktop {
        root.join("desktop")
    } else {
        root
    })?;
    match args.command {
        Command::Serve { listen, ipc_port } => {
            anyhow::ensure!(
                listen.ip().is_loopback(),
                "Bind to loopback and use an HTTPS proxy for remote access"
            );
            let listener = tokio::net::TcpListener::bind(listen).await?;
            eprintln!(
                "Direct instance gateway on {listen}. Use an HTTPS proxy and generate a key with zeron-remote pair."
            );
            let ipc = format!("ws://127.0.0.1:{ipc_port}");
            if args.desktop {
                zeron_rpc::remote::serve_desktop(listener, ipc, store).await?;
            } else {
                zeron_rpc::remote::serve(listener, ipc, store).await?;
            }
        }
        Command::Pair => println!("{}", store.pairing_key()?),
        Command::Devices => {
            for device in store.devices()? {
                println!("{}  {}", device.id, device.name);
            }
        }
        Command::Revoke { id } => store.revoke(&id)?,
    }
    Ok(())
}
