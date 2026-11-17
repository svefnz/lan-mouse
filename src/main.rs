use env_logger::Env;
use input_capture::InputCaptureError;
use input_emulation::InputEmulationError;
use lan_mouse::{
    capture_test,
    config::{self, Command, Config, ConfigError},
    emulation_test,
    service::{Service, ServiceError},
};
use lan_mouse_cli::CliError;
#[cfg(feature = "gtk")]
use lan_mouse_gtk::GtkError;
use lan_mouse_ipc::{IpcError, IpcListenerCreationError};
use std::{env, future::Future, io, process};
#[cfg(all(feature = "gtk", not(feature = "mygo")))]
use std::process::Child;
use thiserror::Error;
use tokio::task::LocalSet;

#[derive(Debug, Error)]
enum LanMouseError {
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error(transparent)]
    IpcError(#[from] IpcError),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Capture(#[from] InputCaptureError),
    #[error(transparent)]
    Emulation(#[from] InputEmulationError),
    #[cfg(feature = "gtk")]
    #[error(transparent)]
    Gtk(#[from] GtkError),
    #[error(transparent)]
    Cli(#[from] CliError),
}

fn main() {
    // init logging
    let env = Env::default().filter_or("LAN_MOUSE_LOG_LEVEL", "info");
    env_logger::init_from_env(env);

    if let Err(e) = run() {
        log::error!("{e}");
        process::exit(1);
    }
}

fn run() -> Result<(), LanMouseError> {
    let config = config::Config::new()?;
    match config.command() {
        Some(command) => match command {
            Command::TestEmulation(args) => run_async(emulation_test::run(config, args))?,
            Command::TestCapture(args) => run_async(capture_test::run(config, args))?,
            Command::Cli(cli_args) => run_async(lan_mouse_cli::run(cli_args))?,
            Command::Daemon => run_daemon(config)?,
        },
        None => {
            // no subcommand: run a graphical frontend
            #[cfg(feature = "mygo")]
            {
                run_go_frontend(config)?;
            }
            #[cfg(not(feature = "mygo"))]
            {
                #[cfg(feature = "gtk")]
                {
                    // Only spawn a new daemon if one isn't already running
                    let mut service = if lan_mouse_ipc::is_service_running() {
                        log::info!("daemon already running, connecting to existing instance");
                        None
                    } else {
                        Some(start_service()?)
                    };
                    let res = lan_mouse_gtk::run(config::local_commit());
                    if let Some(ref mut service) = service {
                        #[cfg(unix)]
                        {
                            // on unix we give the service a chance to terminate gracefully
                            let pid = service.id() as libc::pid_t;
                            unsafe {
                                libc::kill(pid, libc::SIGINT);
                            }
                            service.wait()?;
                        }
                        service.kill()?;
                    }
                    res?;
                }
                #[cfg(not(feature = "gtk"))]
                {
                    run_daemon(config)?
                }
            }
        }
    }

    Ok(())
}

/// run the daemon subcommand
fn run_daemon(config: Config) -> Result<(), LanMouseError> {
    match run_async(run_service(config)) {
        Err(LanMouseError::Service(ServiceError::IpcListen(
            IpcListenerCreationError::AlreadyRunning,
        ))) => {
            log::info!("service already running!");
            Ok(())
        }
        r => r,
    }
}

/// Run the Go frontend (design D3): a sibling `lan-mouse-ui` executable,
/// overridable with `LAN_MOUSE_UI`. The UI executable owns the daemon
/// lifecycle (design D4). If the executable is missing, fall back to
/// running the daemon directly so the user is not left without a service.
#[cfg(feature = "mygo")]
fn run_go_frontend(config: Config) -> Result<(), LanMouseError> {
    let Some(ui) = find_ui() else {
        log::warn!(
            "lan-mouse-ui not found next to the lan-mouse binary (set LAN_MOUSE_UI \
             to override), running the daemon directly"
        );
        return run_daemon(config);
    };
    let mut child = process::Command::new(&ui).spawn()?;
    let status = child.wait()?;
    if !status.success() {
        log::warn!("frontend exited with {status}");
    }
    Ok(())
}

/// locate the Go frontend executable: `LAN_MOUSE_UI` overrides the
/// sibling of the running binary
#[cfg(feature = "mygo")]
fn find_ui() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;

    if let Ok(from_env) = env::var("LAN_MOUSE_UI") {
        let path = PathBuf::from(from_env);
        if path.exists() {
            return Some(path);
        }
        log::warn!("LAN_MOUSE_UI does not point at an existing file, using the sibling");
    }
    let exe = env::current_exe().ok()?;
    let sibling = if cfg!(windows) {
        exe.with_file_name("lan-mouse-ui.exe")
    } else {
        exe.with_file_name("lan-mouse-ui")
    };
    sibling.exists().then_some(sibling)
}

fn run_async<F, E>(f: F) -> Result<(), LanMouseError>
where
    F: Future<Output = Result<(), E>>,
    LanMouseError: From<E>,
{
    // create single threaded tokio runtime
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?;

    // run async event loop
    Ok(runtime.block_on(LocalSet::new().run_until(f))?)
}

/// Spawn the service as a child of this process. Only the GTK frontend
/// runs in-process; the Go frontend owns its own daemon lifecycle.
#[cfg(all(feature = "gtk", not(feature = "mygo")))]
fn start_service() -> Result<Child, io::Error> {
    let child = process::Command::new(std::env::current_exe()?)
        .args(std::env::args().skip(1))
        .arg("daemon")
        .spawn()?;
    Ok(child)
}

async fn run_service(config: Config) -> Result<(), ServiceError> {
    let release_bind = config.release_bind();
    let config_path = config.config_path().to_owned();
    let mut service = Service::new(config).await?;
    log::info!("using config: {config_path:?}");
    log::info!("Press {release_bind:?} to release the mouse");
    service.run().await?;
    log::info!("service exited!");
    Ok(())
}
