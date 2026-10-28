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
use std::{
    env,
    fs::{self, File, OpenOptions},
    future::Future,
    io,
    path::PathBuf,
    process::{self, Child},
};
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

/// the log file is started over once it grows past this, keeping one old one
const MAX_LOG_SIZE: u64 = 5 * 1024 * 1024;

fn main() {
    init_logging();

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
            Command::Daemon => {
                // if daemon is specified we run the service
                match run_async(run_service(config)) {
                    Err(LanMouseError::Service(ServiceError::IpcListen(
                        IpcListenerCreationError::AlreadyRunning,
                    ))) => log::info!("service already running!"),
                    r => r?,
                }
            }
        },
        None => {
            //  otherwise start the service as a child process and
            //  run a frontend
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
                // run daemon if gtk is diabled
                match run_async(run_service(config)) {
                    Err(LanMouseError::Service(ServiceError::IpcListen(
                        IpcListenerCreationError::AlreadyRunning,
                    ))) => log::info!("service already running!"),
                    r => r?,
                }
            }
        }
    }

    Ok(())
}

fn log_path() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    let dir = PathBuf::from(env::var_os("HOME")?).join("Library/Logs/MouseBridge");
    #[cfg(windows)]
    let dir = PathBuf::from(env::var_os("LOCALAPPDATA")?).join("mousebridge");
    #[cfg(all(unix, not(target_os = "macos")))]
    let dir = env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(env::var_os("HOME")?).join(".local/state")))?
        .join("mousebridge");
    Some(dir.join("mousebridge.log"))
}

/// Logs to stderr and to a file that outlives a restart, since an input
/// capture that never lets go may leave nothing but a reboot.
fn init_logging() {
    let env = || Env::default().filter_or("LAN_MOUSE_LOG_LEVEL", "info");
    let stderr = env_logger::Builder::from_env(env()).build();
    let file = open_log_file().map(|file| {
        env_logger::Builder::from_env(env())
            .target(env_logger::Target::Pipe(Box::new(file)))
            .write_style(env_logger::WriteStyle::Never)
            .build()
    });
    log::set_max_level(stderr.filter());
    log::set_boxed_logger(Box::new(Logger { stderr, file })).expect("logger set once");
}

/// Passes the file at most info records: debug and trace ones include the
/// keys typed, which must not pile up on disk.
struct Logger {
    stderr: env_logger::Logger,
    file: Option<env_logger::Logger>,
}

impl log::Log for Logger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        self.stderr.enabled(metadata)
    }

    fn log(&self, record: &log::Record) {
        self.stderr.log(record);
        if record.level() <= log::Level::Info {
            if let Some(file) = &self.file {
                file.log(record);
            }
        }
    }

    fn flush(&self) {
        self.stderr.flush();
        if let Some(file) = &self.file {
            file.flush();
        }
    }
}

fn open_log_file() -> Option<File> {
    let path = log_path()?;
    let mut dir = fs::DirBuilder::new();
    dir.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut dir, 0o700);
    dir.create(path.parent()?).ok()?;
    if fs::metadata(&path).is_ok_and(|m| m.len() > MAX_LOG_SIZE) {
        let _ = fs::rename(&path, path.with_extension("log.1"));
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options.open(&path).ok()?;
    // only the owner may read it, also when an older version created it
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path.parent()?, fs::Permissions::from_mode(0o700));
        let _ = fs::set_permissions(&path, fs::Permissions::from_mode(0o600));
    }
    Some(file)
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
