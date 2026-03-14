# vusb

Swap USB peripherals between remote machines via USB/IP

## Usage

USB/IP needs to be installed first. Make sure the USB/IP daemon (usbipd) is running on the server.

Create a file called `config.toml` in the same directory as vusb and configure it:

```toml
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
```

Next, run `vusb` as the superuser/with administrator privileges. (This is required on Linux to monitor evdev inputs, optional on Windows to automatically bind configured devices with usbipd.)

You can then press LCTRL+RCTRL to swap devices between machines.

