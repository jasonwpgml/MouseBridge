use crate::capture_test::TestCaptureArgs;
use crate::emulation_test::TestEmulationArgs;
use clap::{Parser, Subcommand, ValueEnum};
use notify::event::ModifyKind;
use notify::{EventKind, RecommendedWatcher, Watcher};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env::{self, VarError};
use std::fmt::Display;
use std::fs::{self, File};
use std::io::Write;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::{collections::HashSet, io};
use thiserror::Error;
use toml;
use toml_edit::{self, DocumentMut};

use lan_mouse_cli::CliArgs;
use lan_mouse_ipc::{DEFAULT_PORT, KeyboardMode, Placement, Position};

use input_event::scancode::{
    self,
    Linux::{KeyLeftAlt, KeyLeftCtrl, KeyLeftMeta, KeyLeftShift},
};

use crate::key_remap::KeyRemap;

use shadow_rs::shadow;

shadow!(build);

/// Local build's 8-byte ASCII short commit hash, suitable for use
/// in [`lan_mouse_proto::ProtoEvent::Hello`]. Pads with `'?'` if
/// shadow_rs returns an unexpected length so the field is always
/// well-formed on the wire.
pub fn local_commit() -> [u8; 8] {
    let bytes = build::SHORT_COMMIT.as_bytes();
    let mut out = [b'?'; 8];
    let n = bytes.len().min(8);
    out[..n].copy_from_slice(&bytes[..n]);
    out
}

const CONFIG_FILE_NAME: &str = "config.toml";
const KEYBOARD_FOLLOW: &str = "follow";
const KEYBOARD_LOCAL: &str = "local";
const CERT_FILE_NAME: &str = "mousebridge.pem";
const CONFIG_DIR_NAME: &str = "mousebridge";
/// where Lan Mouse, which MouseBridge is based on, keeps its files
const LAN_MOUSE_DIR_NAME: &str = "lan-mouse";
const LAN_MOUSE_CERT_FILE_NAME: &str = "lan-mouse.pem";

fn default_path() -> Result<PathBuf, VarError> {
    #[cfg(unix)]
    let base = env::var("XDG_CONFIG_HOME").unwrap_or(format!("{}/.config", env::var("HOME")?));

    #[cfg(not(unix))]
    let base = env::var("LOCALAPPDATA").unwrap_or(format!("{}/.config", env::var("USERPROFILE")?));

    Ok(PathBuf::from(base).join(CONFIG_DIR_NAME))
}

fn parse_keyboard_mode(value: Option<&str>) -> KeyboardMode {
    match value {
        None | Some(KEYBOARD_FOLLOW) => KeyboardMode::FollowPointer,
        Some(KEYBOARD_LOCAL) => KeyboardMode::Local,
        Some(hostname) => KeyboardMode::Device(hostname.to_owned()),
    }
}

fn keyboard_mode_value(mode: &KeyboardMode) -> Option<String> {
    match mode {
        KeyboardMode::FollowPointer => None,
        KeyboardMode::Local => Some(KEYBOARD_LOCAL.to_owned()),
        KeyboardMode::Device(hostname) => Some(hostname.clone()),
    }
}

/// Copies a Lan Mouse config and certificate into `dir` when it has no config
/// yet, so devices that trust this one keep recognizing it.
fn import_lan_mouse_config(dir: &Path) -> io::Result<()> {
    let Some(old) = dir.parent().map(|p| p.join(LAN_MOUSE_DIR_NAME)) else {
        return Ok(());
    };
    if dir.join(CONFIG_FILE_NAME).exists() || !old.join(CONFIG_FILE_NAME).exists() {
        return Ok(());
    }
    fs::create_dir_all(dir)?;
    fs::copy(old.join(CONFIG_FILE_NAME), dir.join(CONFIG_FILE_NAME))?;
    let old_cert = old.join(LAN_MOUSE_CERT_FILE_NAME);
    if old_cert.exists() && !dir.join(CERT_FILE_NAME).exists() {
        fs::copy(old_cert, dir.join(CERT_FILE_NAME))?;
    }
    log::info!("imported the Lan Mouse configuration from {old:?}");
    Ok(())
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
struct ConfigToml {
    capture_backend: Option<CaptureBackend>,
    emulation_backend: Option<EmulationBackend>,
    port: Option<u16>,
    release_bind: Option<Vec<scancode::Linux>>,
    cert_path: Option<PathBuf>,
    /// show remote pointers as cursors of their own (Windows only)
    second_cursor: Option<bool>,
    /// where keyboard input goes: "follow" (default), "local" or a hostname
    keyboard: Option<String>,
    /// where this device sits on the canvas the devices are arranged on
    placement: Option<Placement>,
    clients: Option<Vec<TomlClient>>,
    authorized_fingerprints: Option<HashMap<String, String>>,
    /// per-client key remapping, keyed by client hostname
    key_remap: Option<HashMap<String, HashMap<scancode::Linux, scancode::Linux>>>,
}

#[derive(Clone, Serialize, Deserialize, Debug, Eq, PartialEq)]
struct TomlClient {
    hostname: Option<String>,
    host_name: Option<String>,
    ips: Option<Vec<IpAddr>>,
    port: Option<u16>,
    position: Option<Position>,
    activate_on_startup: Option<bool>,
    enter_hook: Option<String>,
    leave_hook: Option<String>,
    placement: Option<Placement>,
}

impl ConfigToml {
    fn new(path: &Path) -> Result<ConfigToml, ConfigError> {
        let config = fs::read_to_string(path)?;
        Ok(toml::from_str::<_>(&config)?)
    }
}

#[derive(Parser, Debug)]
#[command(author, version=build::CLAP_LONG_VERSION, about, long_about = None)]
struct Args {
    /// the listen port for lan-mouse
    #[arg(short, long)]
    port: Option<u16>,

    /// non-default config file location
    #[arg(short, long)]
    config: Option<PathBuf>,

    /// capture backend override
    #[arg(long)]
    capture_backend: Option<CaptureBackend>,

    /// emulation backend override
    #[arg(long)]
    emulation_backend: Option<EmulationBackend>,

    /// path to non-default certificate location
    #[arg(long)]
    cert_path: Option<PathBuf>,

    /// subcommands
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand, Clone, Debug, Eq, PartialEq)]
pub enum Command {
    /// test input emulation
    TestEmulation(TestEmulationArgs),
    /// test input capture
    TestCapture(TestCaptureArgs),
    /// Lan Mouse commandline interface
    Cli(CliArgs),
    /// run in daemon mode
    Daemon,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
pub enum CaptureBackend {
    #[cfg(libei_capture)]
    #[serde(rename = "input-capture-portal")]
    InputCapturePortal,
    #[cfg(layer_shell_capture)]
    #[serde(rename = "layer-shell")]
    LayerShell,
    #[cfg(x11_capture)]
    #[serde(rename = "x11")]
    X11,
    #[cfg(windows)]
    #[serde(rename = "windows")]
    Windows,
    #[cfg(target_os = "macos")]
    #[serde(rename = "macos")]
    MacOs,
    #[serde(rename = "dummy")]
    Dummy,
}

impl Display for CaptureBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(libei_capture)]
            CaptureBackend::InputCapturePortal => write!(f, "input-capture-portal"),
            #[cfg(layer_shell_capture)]
            CaptureBackend::LayerShell => write!(f, "layer-shell"),
            #[cfg(x11_capture)]
            CaptureBackend::X11 => write!(f, "X11"),
            #[cfg(windows)]
            CaptureBackend::Windows => write!(f, "windows"),
            #[cfg(target_os = "macos")]
            CaptureBackend::MacOs => write!(f, "MacOS"),
            CaptureBackend::Dummy => write!(f, "dummy"),
        }
    }
}

impl From<CaptureBackend> for input_capture::Backend {
    fn from(backend: CaptureBackend) -> Self {
        match backend {
            #[cfg(libei_capture)]
            CaptureBackend::InputCapturePortal => Self::InputCapturePortal,
            #[cfg(layer_shell_capture)]
            CaptureBackend::LayerShell => Self::LayerShell,
            #[cfg(x11_capture)]
            CaptureBackend::X11 => Self::X11,
            #[cfg(windows)]
            CaptureBackend::Windows => Self::Windows,
            #[cfg(target_os = "macos")]
            CaptureBackend::MacOs => Self::MacOs,
            CaptureBackend::Dummy => Self::Dummy,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize, ValueEnum)]
pub enum EmulationBackend {
    #[cfg(wlroots_emulation)]
    #[serde(rename = "wlroots")]
    Wlroots,
    #[cfg(libei_emulation)]
    #[serde(rename = "libei")]
    Libei,
    #[cfg(rdp_emulation)]
    #[serde(rename = "xdp")]
    Xdp,
    #[cfg(x11_emulation)]
    #[serde(rename = "x11")]
    X11,
    #[cfg(windows)]
    #[serde(rename = "windows")]
    Windows,
    #[cfg(target_os = "macos")]
    #[serde(rename = "macos")]
    MacOs,
    #[serde(rename = "dummy")]
    Dummy,
}

impl From<EmulationBackend> for input_emulation::Backend {
    fn from(backend: EmulationBackend) -> Self {
        match backend {
            #[cfg(wlroots_emulation)]
            EmulationBackend::Wlroots => Self::Wlroots,
            #[cfg(libei_emulation)]
            EmulationBackend::Libei => Self::Libei,
            #[cfg(rdp_emulation)]
            EmulationBackend::Xdp => Self::Xdp,
            #[cfg(x11_emulation)]
            EmulationBackend::X11 => Self::X11,
            #[cfg(windows)]
            EmulationBackend::Windows => Self::Windows,
            #[cfg(target_os = "macos")]
            EmulationBackend::MacOs => Self::MacOs,
            EmulationBackend::Dummy => Self::Dummy,
        }
    }
}

impl Display for EmulationBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(wlroots_emulation)]
            EmulationBackend::Wlroots => write!(f, "wlroots"),
            #[cfg(libei_emulation)]
            EmulationBackend::Libei => write!(f, "libei"),
            #[cfg(rdp_emulation)]
            EmulationBackend::Xdp => write!(f, "xdg-desktop-portal"),
            #[cfg(x11_emulation)]
            EmulationBackend::X11 => write!(f, "X11"),
            #[cfg(windows)]
            EmulationBackend::Windows => write!(f, "windows"),
            #[cfg(target_os = "macos")]
            EmulationBackend::MacOs => write!(f, "macos"),
            EmulationBackend::Dummy => write!(f, "dummy"),
        }
    }
}

#[derive(Debug)]
pub struct Config {
    /// command line arguments
    args: Args,
    /// path to the certificate file used
    cert_path: PathBuf,
    /// path to the config file used
    config_path: PathBuf,
    /// path to config directory (parent of above)
    config_dir: PathBuf,
    /// the (optional) toml config and it's path
    config_toml: Option<ConfigToml>,
    // filesystem watcher
    watcher: notify::RecommendedWatcher,
    // channel for filesystem events
    watch_rx: tokio::sync::mpsc::Receiver<Result<notify::Event, notify::Error>>,
}

pub struct ConfigClient {
    pub ips: HashSet<IpAddr>,
    pub hostname: Option<String>,
    pub port: u16,
    pub pos: Position,
    pub active: bool,
    pub enter_hook: Option<String>,
    pub leave_hook: Option<String>,
    pub placement: Option<Placement>,
}

impl From<TomlClient> for ConfigClient {
    fn from(toml: TomlClient) -> Self {
        let active = toml.activate_on_startup.unwrap_or(false);
        let enter_hook = toml.enter_hook;
        let leave_hook = toml.leave_hook;
        let hostname = toml.hostname;
        let ips = HashSet::from_iter(toml.ips.into_iter().flatten());
        let port = toml.port.unwrap_or(DEFAULT_PORT);
        let pos = toml.position.unwrap_or_default();
        Self {
            ips,
            hostname,
            port,
            pos,
            active,
            enter_hook,
            leave_hook,
            placement: toml.placement,
        }
    }
}

impl From<ConfigClient> for TomlClient {
    fn from(client: ConfigClient) -> Self {
        let hostname = client.hostname;
        let host_name = None;
        let mut ips = client.ips.into_iter().collect::<Vec<_>>();
        ips.sort();
        let ips = Some(ips);
        let port = if client.port == DEFAULT_PORT {
            None
        } else {
            Some(client.port)
        };
        let position = Some(client.pos);
        let activate_on_startup = if client.active { Some(true) } else { None };
        let enter_hook = client.enter_hook;
        let leave_hook = client.leave_hook;
        Self {
            hostname,
            host_name,
            ips,
            port,
            position,
            activate_on_startup,
            enter_hook,
            leave_hook,
            placement: client.placement,
        }
    }
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error(transparent)]
    Toml(#[from] toml::de::Error),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Var(#[from] VarError),
    #[error(transparent)]
    Watcher(#[from] notify::Error),
}

const DEFAULT_RELEASE_KEYS: [scancode::Linux; 4] =
    [KeyLeftCtrl, KeyLeftShift, KeyLeftMeta, KeyLeftAlt];

impl Config {
    pub fn new() -> Result<Self, ConfigError> {
        let args = Args::parse();

        if args.config.is_none() {
            if let Err(e) = import_lan_mouse_config(&default_path()?) {
                log::warn!("could not import the Lan Mouse configuration: {e}");
            }
        }

        // --config <file> overrules default location
        let config_path = args
            .config
            .clone()
            .unwrap_or(default_path()?.join(CONFIG_FILE_NAME));
        let config_dir = config_path
            .parent()
            .expect("config directory")
            .to_path_buf();

        // Ensure the config directory exists and write a default config file
        // if none is present. Runs on every Config::new(), regardless of which
        // entry path (GUI main, spawned daemon, CLI, test commands) we're on,
        // so a fresh Mac never hits "No such file or directory" on config.toml
        // and notify::Watcher (which requires the dir to exist on macOS
        // FSEvents and some Linux backends) has a concrete path to watch.
        fs::create_dir_all(&config_dir)?;
        if !config_path.exists() {
            let default_toml = toml::to_string_pretty(&ConfigToml::default())
                .expect("default ConfigToml serialization cannot fail");
            fs::write(&config_path, default_toml)?;
        }

        let config_toml = match ConfigToml::new(&config_path) {
            Err(e) => {
                log::warn!("{config_path:?}: {e}");
                log::warn!("Continuing without config file ...");
                None
            }
            Ok(c) => Some(c),
        };

        // --cert-path <file> overrules default location
        let cert_path = args
            .cert_path
            .clone()
            .or(config_toml.as_ref().and_then(|c| c.cert_path.clone()))
            .unwrap_or(default_path()?.join(CERT_FILE_NAME));

        let (tx, watch_rx) = tokio::sync::mpsc::channel(16);
        let watcher = RecommendedWatcher::new(
            move |res| {
                let _ = tx.blocking_send(res);
            },
            notify::Config::default(),
        )?;
        let mut config = Config {
            args,
            cert_path,
            config_path,
            config_dir,
            config_toml,
            watcher,
            watch_rx,
        };
        config.watch()?;
        Ok(config)
    }

    fn watch(&mut self) -> Result<(), notify::Error> {
        self.watcher
            .watch(&self.config_dir, notify::RecursiveMode::NonRecursive)?;
        Ok(())
    }

    fn unwatch(&mut self) -> Result<(), notify::Error> {
        self.watcher.unwatch(&self.config_dir)?;
        Ok(())
    }

    pub async fn changed(&mut self) -> Result<(), notify::Error> {
        loop {
            let event = self.watch_rx.recv().await.expect("channel closed");
            let event = event.expect("filesystem event");
            if event.paths.contains(&self.config_path)
                && matches!(
                    event.kind,
                    EventKind::Create(_)
                        | EventKind::Modify(ModifyKind::Data(_))
                        | EventKind::Remove(_)
                )
                && self.read_from_disk()?
            {
                return Ok(());
            }
        }
    }

    /// the command to run
    pub fn command(&self) -> Option<Command> {
        self.args.command.clone()
    }

    pub fn config_path(&self) -> &Path {
        &self.config_path
    }

    /// public key fingerprints authorized for connection
    pub fn authorized_fingerprints(&self) -> HashMap<String, String> {
        self.config_toml
            .as_ref()
            .and_then(|c| c.authorized_fingerprints.clone())
            .unwrap_or_default()
    }

    /// path to certificate
    pub fn cert_path(&self) -> &Path {
        &self.cert_path
    }

    /// optional input-capture backend override
    pub fn capture_backend(&self) -> Option<CaptureBackend> {
        self.args
            .capture_backend
            .or(self.config_toml.as_ref().and_then(|c| c.capture_backend))
    }

    /// optional input-emulation backend override
    pub fn emulation_backend(&self) -> Option<EmulationBackend> {
        self.args
            .emulation_backend
            .or(self.config_toml.as_ref().and_then(|c| c.emulation_backend))
    }

    /// the port to use (initially)
    pub fn port(&self) -> u16 {
        self.args
            .port
            .or(self.config_toml.as_ref().and_then(|c| c.port))
            .unwrap_or(DEFAULT_PORT)
    }

    /// list of configured clients
    pub fn clients(&self) -> Vec<ConfigClient> {
        self.config_toml
            .as_ref()
            .map(|c| c.clients.clone())
            .unwrap_or_default()
            .into_iter()
            .flatten()
            .map(From::<TomlClient>::from)
            .collect()
    }

    /// release bind for returning control to the host
    pub fn release_bind(&self) -> Vec<scancode::Linux> {
        self.config_toml
            .as_ref()
            .and_then(|c| c.release_bind.clone())
            .unwrap_or(Vec::from_iter(DEFAULT_RELEASE_KEYS.iter().cloned()))
    }

    /// key remapping for the client with the given hostname
    pub fn key_remap(&self, hostname: &str) -> Option<KeyRemap> {
        self.config_toml
            .as_ref()?
            .key_remap
            .as_ref()?
            .iter()
            .find(|(h, _)| h.eq_ignore_ascii_case(hostname))
            .map(|(_, map)| KeyRemap::new(map))
    }

    /// where this device's keyboard input goes
    pub fn keyboard_mode(&self) -> KeyboardMode {
        parse_keyboard_mode(
            self.config_toml
                .as_ref()
                .and_then(|c| c.keyboard.as_deref()),
        )
    }

    /// where this device sits on the canvas
    pub fn placement(&self) -> Placement {
        self.config_toml
            .as_ref()
            .and_then(|c| c.placement)
            .unwrap_or_default()
    }

    pub fn set_placement(&mut self, placement: Placement) {
        self.config_toml.get_or_insert_default().placement = Some(placement);
    }

    pub fn set_keyboard_mode(&mut self, mode: &KeyboardMode) {
        self.config_toml.get_or_insert_default().keyboard = keyboard_mode_value(mode);
    }

    /// With `second_cursor` on, the labels for remote pointers: the hostname
    /// of the client configured with that address.
    pub fn second_cursor_labels(&self) -> Option<HashMap<IpAddr, String>> {
        if !self.config_toml.as_ref()?.second_cursor? {
            return None;
        }
        let mut labels = HashMap::new();
        for client in self.clients() {
            if let Some(hostname) = client.hostname {
                for ip in client.ips {
                    labels.insert(ip, hostname.clone());
                }
            }
        }
        Some(labels)
    }

    /// set configured clients
    pub fn set_clients(&mut self, clients: Vec<ConfigClient>) {
        if clients.is_empty() {
            return;
        }
        if self.config_toml.is_none() {
            self.config_toml = Some(Default::default());
        }
        self.config_toml.as_mut().expect("config").clients =
            Some(clients.into_iter().map(|c| c.into()).collect::<Vec<_>>());
    }

    /// set authorized keys
    pub fn set_authorized_keys(&mut self, fingerprints: HashMap<String, String>) {
        if self.config_toml.is_none() {
            self.config_toml = Some(Default::default());
        }
        self.config_toml
            .as_mut()
            .expect("config")
            .authorized_fingerprints = Some(fingerprints);
    }

    pub fn read_from_disk(&mut self) -> Result<bool, io::Error> {
        log::info!("reading config from {:?}", self.config_path);

        let current_config = fs::read_to_string(&self.config_path)?;
        let current_config = match current_config.parse::<DocumentMut>() {
            Ok(c) => c,
            Err(e) => {
                log::warn!("{:?} {e}", self.config_path());
                return Ok(false);
            }
        };
        let mut changed = false;
        match toml_edit::de::from_document::<ConfigToml>(current_config) {
            Ok(current_config) => {
                changed = self
                    .config_toml
                    .as_ref()
                    .is_none_or(|c| c != &current_config);
                self.config_toml.replace(current_config);
            }
            Err(e) => log::warn!("{:?} {e}", self.config_path()),
        };
        if changed {
            log::info!("config changed");
        } else {
            log::info!("config unchanged");
        }
        Ok(changed)
    }

    pub fn write_back(&mut self) -> Result<(), io::Error> {
        log::info!("writing config to {:?}", self.config_path);
        /* the new config */
        let new_config = self.config_toml.clone().unwrap_or_default();
        let new_config = toml_edit::ser::to_string_pretty(&new_config).expect("config");

        /*
         * TODO merge with current config file to preserve comments
         * => eventually we might want to split this up into clients configured
         * via the config file and clients managed through the GUI / frontend.
         * The latter should be saved to $XDG_DATA_HOME instead of $XDG_CONFIG_HOME,
         * and clients configured through .config could be made permanent.
         * For now we just override the config file.
         */

        let _ = self.unwatch();
        /* write new config to file */
        if let Some(p) = self.config_path().parent() {
            fs::create_dir_all(p)?;
        }
        {
            let mut f = File::create(self.config_path())?;
            f.write_all(new_config.as_bytes())?;
            f.sync_all()?;
        }

        let _ = self.watch();

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY_REMAP_TOML: &str = r#"
second_cursor = true

[key_remap.DESKTOP-SN73G0N]
KeyLeftMeta = "KeyLeftCtrl"
KeyF18 = "KeyHanguel"
"#;

    #[test]
    fn placements_survive_write_back() {
        let doc = r#"
placement = { x = 200, y = 1080 }

[[clients]]
hostname = "DESKTOP-SN73G0N"
placement = { x = 0, y = 0, size = 80 }
"#
        .parse::<DocumentMut>()
        .unwrap();
        let config: ConfigToml = toml_edit::de::from_document(doc).unwrap();
        // size defaults to the real size
        assert_eq!(
            config.placement,
            Some(Placement {
                x: 200,
                y: 1080,
                size: 100
            })
        );
        let written = toml_edit::ser::to_string_pretty(&config).unwrap();
        let doc = written.parse::<DocumentMut>().unwrap();
        let read: ConfigToml = toml_edit::de::from_document(doc).unwrap();
        assert_eq!(read, config);
        let client = &read.clients.unwrap()[0];
        assert_eq!(client.placement.map(|p| p.size), Some(80));
    }

    #[test]
    fn parses_key_remap() {
        let doc = KEY_REMAP_TOML.parse::<DocumentMut>().unwrap();
        let config: ConfigToml = toml_edit::de::from_document(doc).unwrap();
        let remap = &config.key_remap.as_ref().unwrap()["DESKTOP-SN73G0N"];
        assert_eq!(remap[&scancode::Linux::KeyF18], scancode::Linux::KeyHanguel);
        assert_eq!(config.second_cursor, Some(true));
    }

    #[test]
    fn key_remap_survives_write_back() {
        let doc = KEY_REMAP_TOML.parse::<DocumentMut>().unwrap();
        let config: ConfigToml = toml_edit::de::from_document(doc).unwrap();
        let written = toml_edit::ser::to_string_pretty(&config).unwrap();
        let doc = written.parse::<DocumentMut>().unwrap();
        let reread: ConfigToml = toml_edit::de::from_document(doc).unwrap();
        assert_eq!(reread, config);
    }

    #[test]
    fn keyboard_mode_round_trips() {
        assert_eq!(parse_keyboard_mode(None), KeyboardMode::FollowPointer);
        assert_eq!(
            parse_keyboard_mode(Some("follow")),
            KeyboardMode::FollowPointer
        );
        for mode in [
            KeyboardMode::Local,
            KeyboardMode::Device("DESKTOP-SN73G0N".into()),
            KeyboardMode::FollowPointer,
        ] {
            let value = keyboard_mode_value(&mode);
            assert_eq!(parse_keyboard_mode(value.as_deref()), mode);
        }
    }

    #[test]
    fn imports_lan_mouse_config_once() {
        let base = env::temp_dir().join(format!("mousebridge-import-{}", std::process::id()));
        let (old, new) = (base.join(LAN_MOUSE_DIR_NAME), base.join(CONFIG_DIR_NAME));
        fs::create_dir_all(&old).unwrap();
        fs::write(old.join(CONFIG_FILE_NAME), "port = 4242\n").unwrap();
        fs::write(old.join(LAN_MOUSE_CERT_FILE_NAME), "cert").unwrap();

        import_lan_mouse_config(&new).unwrap();
        assert_eq!(
            fs::read_to_string(new.join(CERT_FILE_NAME)).unwrap(),
            "cert"
        );

        // an existing MouseBridge config is left alone
        fs::write(new.join(CONFIG_FILE_NAME), "port = 5000\n").unwrap();
        import_lan_mouse_config(&new).unwrap();
        let config = fs::read_to_string(new.join(CONFIG_FILE_NAME)).unwrap();
        fs::remove_dir_all(&base).unwrap();
        assert_eq!(config, "port = 5000\n");
    }
}
