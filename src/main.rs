use getopts::Options;
use serde::Deserialize;
use std::env;
use std::fs;
use std::sync::mpsc::{self, Sender};
use std::thread;

use crate::network::Event;

mod network;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
mod linux;

const DEFAULT_USBIP_PORT: u16 = 3240;

const DEFAULT_CONFIG: &'static str = r#"
# Uncomment if usbip (client) or usbipd (server) aren't in PATH
# usbip_binary = "/usr/bin/usbip"
# usbipd_binary = "/usr/bin/usbipd"

# Uncomment the following if running as server:
# [server]
# addresses = ["0.0.0.0", "::"]
# port = 3340
# devices = []

# Add your USB device busid strings to bind/attach above (see `usbipd list`)

# Uncomment the following if running as client:
# [client]
# address = "192.168.1.x"
# port = 3340
# usbip_port = 3240

# Alternatively, you can uncomment both sections and force vusb
# to run as either using the "-s" or "-c" options.
"#;

#[derive(Deserialize)]
struct Config {
    usbip_binary: Option<String>,
    usbipd_binary: Option<String>,
    server: Option<ServerConfig>,
    client: Option<ClientConfig>,
}

#[derive(Deserialize)]
struct ServerConfig {
    addresses: Vec<String>,
    port: u16,
    devices: Vec<String>,
}

#[derive(Deserialize)]
struct ClientConfig {
    address: String,
    port: u16,
    usbip_port: Option<u16>,
}

fn print_usage(program: &str, opts: Options) {
    let brief = format!("usage: {} [options]", program);
    print!("{}", opts.usage(&brief));
}

fn spawn_keyhandler(sender: Sender<Event>) {
    #[cfg(target_os = "linux")]
    thread::spawn(|| {
        linux::KeyHandler::new(&[evdev::KeyCode::KEY_LEFTCTRL, evdev::KeyCode::KEY_RIGHTCTRL])
            .expect("failed to create hook")
            .run(sender)
            .expect("hook failed");
    });

    #[cfg(target_os = "windows")]
    thread::spawn(|| {
        if !windows::KeyHandler::install() {
            panic!("failed to create hook");
        }

        windows::KeyHandler::run(sender);
    });

    println!("created hook");
}

fn main() {
    let args: Vec<String> = env::args().collect();
    let program = args[0].clone();

    let mut opts = Options::new();
    opts.optflag("s", "server", "run as client");
    opts.optflag("c", "client", "run as client");
    opts.optflag("h", "help", "print this help menu");

    let matches = match opts.parse(&args[1..]) {
        Ok(m) => m,
        Err(f) => {
            panic!("{}", f.to_string())
        }
    };

    let is_server = matches.opt_present("s");
    let is_client = matches.opt_present("c");
    if matches.opt_present("h") || (is_server && is_client) {
        print_usage(&program, opts);
        return;
    }

    let mut config_path = env::current_exe().unwrap();
    config_path.pop();
    config_path.push("config.toml");

    if let Ok(contents) = fs::read_to_string(config_path.clone())
        && let Ok(config) = toml::from_str::<Config>(&contents)
    {
        if !is_client && let Some(server_config) = config.server {
            println!("running server...");

            let (mut server, sender) =
                network::Server::bind(server_config.addresses, server_config.port)
                    .expect("failed to create server");

            spawn_keyhandler(sender);

            server
                .run(
                    &config.usbipd_binary.unwrap_or("usbipd".into()),
                    &server_config.devices,
                )
                .unwrap();
        } else if !is_server && let Some(client_config) = config.client {
            println!("running client...");

            let mut client = network::Client::connect(
                &client_config.address,
                client_config.port,
                client_config.usbip_port.unwrap_or(DEFAULT_USBIP_PORT),
            )
            .expect("failed to connect");
            let (sender, receiver) = mpsc::channel::<network::Event>();

            spawn_keyhandler(sender);

            client
                .run(receiver, &config.usbip_binary.unwrap_or("usbip".into()))
                .unwrap();
        } else {
            println!("did not start either server or client, exiting");
        }
    } else {
        println!("error: config.toml is invalid, please check your configuration");

        if let Ok(exists) = fs::exists(config_path.clone())
            && !exists
        {
            fs::write(config_path, DEFAULT_CONFIG).unwrap();
        }
    }
}
