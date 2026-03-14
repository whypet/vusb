use evdev::{Device, EventType, KeyCode};
use inotify::Inotify;
use inotify::WatchMask;
use nix::errno::Errno;
use nix::libc;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use std::fs;
use std::io;
use std::os::fd::AsFd;
use std::sync::mpsc::Sender;
use thiserror::Error;

use crate::network::Event;

#[derive(Error, Debug)]
pub enum HookError {
    #[error("IO error")]
    Io(#[from] io::Error),
    #[error("Poll error")]
    Poll(#[from] Errno),
}

pub struct Hook {
    keycodes: Vec<KeyCode>,
    down: Vec<KeyCode>,
    inotify: Inotify,
    devices: Vec<Device>,
    scan: bool,
    active: bool,
}

impl Hook {
    pub fn new(keycodes: &[KeyCode]) -> Result<Hook, HookError> {
        let inotify = Inotify::init()?;
        inotify
            .watches()
            .add("/dev/input", WatchMask::CREATE | WatchMask::ATTRIB)?;

        let devices = Self::scan()?;

        Ok(Hook {
            keycodes: keycodes.into(),
            down: Vec::new(),
            inotify,
            devices,
            scan: false,
            active: false,
        })
    }

    fn scan() -> Result<Vec<Device>, HookError> {
        let paths = fs::read_dir("/dev/input")?.filter_map(|entry| {
            let path = entry.ok()?.path();
            if path.file_name()?.to_str()?.starts_with("event") {
                Some(path)
            } else {
                None
            }
        });

        let mut devices: Vec<Device> = Vec::new();

        for path in paths {
            if let Ok(device) = Device::open(&path) {
                if device
                    .supported_keys()
                    .map_or(false, |keys| keys.contains(KeyCode::KEY_ENTER))
                {
                    devices.push(device);
                }
            }
        }

        Ok(devices)
    }

    pub fn run(&mut self, sender: Sender<Event>) -> Result<(), HookError> {
        loop {
            if self.scan {
                self.scan = false;
                self.devices = Self::scan()?;
            }

            let (inotify_ready, devices_ready) = {
                let mut pollfds = vec![PollFd::new(self.inotify.as_fd(), PollFlags::POLLIN)];

                for device in &self.devices {
                    pollfds.push(PollFd::new(device.as_fd(), PollFlags::POLLIN));
                }

                poll(&mut pollfds, PollTimeout::NONE)?;

                let inotify_ready = pollfds[0].any().unwrap_or_default();
                let devices_ready: Vec<bool> = pollfds[1..]
                    .iter()
                    .map(|p| p.any().unwrap_or_default())
                    .collect();

                (inotify_ready, devices_ready)
            };

            if inotify_ready {
                let mut inotify_buf = [0; 1024];
                if let Ok(events) = self.inotify.read_events(&mut inotify_buf) {
                    for _ in events {
                        self.scan = true;
                    }
                }
            }

            let mut disconnected: Vec<usize> = Vec::new();

            for (i, device) in self.devices.iter_mut().enumerate() {
                if devices_ready[i] {
                    match device.fetch_events() {
                        Ok(events) => {
                            for event in events {
                                if event.event_type() == EventType::KEY {
                                    let key = KeyCode::new(event.code());

                                    match event.value() {
                                        0 => {
                                            self.down.retain(|&k| k != key);
                                            if self.down.is_empty() && self.active {
                                                self.active = false;
                                                sender.send(Event::Activated).ok();
                                            }
                                        }
                                        1 => {
                                            if !self.down.contains(&key) {
                                                self.down.push(key);
                                            }

                                            let active = self
                                                .keycodes
                                                .iter()
                                                .all(|k| self.down.contains(&k));

                                            if active && !self.active {
                                                self.active = true;
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        Err(e) if e.raw_os_error() == Some(libc::ENODEV) => {
                            disconnected.push(i);
                        }
                        Err(e) => eprintln!("Error reading device: {}", e),
                    }
                }
            }

            for i in disconnected.iter().rev() {
                self.devices.remove(*i);
            }
        }
    }
}
