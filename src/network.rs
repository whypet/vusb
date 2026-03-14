use std::io::prelude::*;
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::Command;
use std::string;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time;
use std::{io, net};
use thiserror::Error;
use wincode::{SchemaRead, SchemaWrite};

#[derive(Error, Debug)]
pub enum NetError {
    #[error("IP address parse error")]
    AddrParse(#[from] net::AddrParseError),
    #[error("IO error")]
    Io(#[from] io::Error),
    #[error("Deserialization error")]
    PacketRead(#[from] wincode::ReadError),
    #[error("Serialization error")]
    PacketWrite(#[from] wincode::WriteError),
    #[error("UTF-8 error")]
    Utf8(#[from] string::FromUtf8Error),
    #[error("Stream end of file")]
    Eof,
}

pub enum Event {
    Activated,
}

#[derive(Debug, SchemaWrite, SchemaRead)]
pub enum Packet {
    Activated,
    Attach { busids: Vec<String> },
    Detach,
}

pub struct Server {
    receiver: mpsc::Receiver<Event>,
    listener: TcpListener,
    clients: Vec<Client>,
    host_index: usize,
}

pub struct Client {
    pub address: SocketAddr,
    pub stream: TcpStream,
    usbip_port: u16,
}

fn read_packet(stream: &mut TcpStream) -> Result<Option<Packet>, NetError> {
    let mut lenbuf = [0u8; 2];

    match stream.peek(&mut lenbuf) {
        Ok(2) => {
            let len = u16::from_le_bytes(lenbuf) as usize + 2;
            let mut buf = vec![0u8; len];

            match stream.peek(&mut buf) {
                Ok(n) if n == len => {
                    stream.read_exact(&mut buf)?;

                    let packet: Packet = wincode::deserialize(&buf[2..])?;

                    println!(
                        "read packet -> {}: {:?}",
                        stream.peer_addr().unwrap(),
                        packet
                    );

                    return Ok(Some(packet));
                }
                Ok(0) => return Err(NetError::Eof),
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(NetError::Io(e)),
            }
        }
        Ok(0) => return Err(NetError::Eof),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
        Err(e) => return Err(NetError::Io(e)),
    }

    Ok(None)
}

fn write_packet(stream: &mut TcpStream, packet: &Packet) -> Result<(), NetError> {
    let encoded: Vec<u8> = wincode::serialize(packet)?;
    let len = encoded.len() as u16;

    stream.write_all(&len.to_le_bytes())?;
    stream.write_all(&encoded)?;

    println!(
        "wrote packet -> {}: {:?}",
        stream.peer_addr().unwrap(),
        packet
    );

    Ok(())
}

impl Server {
    pub fn bind(addresses: Vec<String>, port: u16) -> Result<(Server, Sender<Event>), NetError> {
        let addresses: Vec<SocketAddr> = addresses
            .iter()
            .map(|a| Ok(SocketAddr::new(a.parse()?, port)))
            .collect::<Result<Vec<_>, NetError>>()?;

        let listener = TcpListener::bind(addresses.as_slice())?;
        let channel = mpsc::channel::<Event>();

        listener.set_nonblocking(true)?;

        println!("bound to: {:?}", addresses);

        Ok((
            Server {
                receiver: channel.1,
                listener,
                clients: Vec::new(),
                host_index: 0,
            },
            channel.0,
        ))
    }

    pub fn run(&mut self, usbipd_binary: &str, busids: &[String]) -> Result<(), NetError> {
        #[cfg(target_os = "linux")]
        {
            // unused
            _ = usbipd_binary;
        }

        #[cfg(target_os = "windows")]
        for busid in busids {
            Command::new(usbipd_binary).args(["bind", busid]).output()?;
        }

        loop {
            let mut cycle_host = false;

            match self.listener.accept() {
                Ok((s, addr)) => {
                    s.set_nonblocking(true)?;
                    self.clients.push(Client {
                        address: addr,
                        stream: s,
                        usbip_port: 0,
                    });
                    println!("stream opened: {}", addr);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(NetError::Io(e)),
            }

            if self.host_index > 0 {
                let mut eof = false;
                let index = self.host_index - 1;
                let client = &mut self.clients[index];

                loop {
                    let res = read_packet(&mut client.stream);

                    if let Ok(Some(packet)) = res {
                        match packet {
                            Packet::Activated => cycle_host = true,
                            _ => {}
                        }
                    } else {
                        match res {
                            Err(NetError::Eof) => eof = true,
                            _ => {}
                        }
                        break;
                    }
                }

                if eof {
                    self.eof(index - 1);
                    if self.host_index >= index + 1 {
                        self.host_index = 0;
                        cycle_host = true;
                    }
                }
            }

            while let Ok(event) = self.receiver.try_recv() {
                match event {
                    Event::Activated => {
                        cycle_host = true;
                    }
                }
            }

            if cycle_host && !self.clients.is_empty() {
                println!("swapping hosts now...");

                if self.host_index > 0 {
                    let mut eof = false;

                    if let Some(old_client) = self.clients.get_mut(self.host_index - 1) {
                        if write_packet(&mut old_client.stream, &Packet::Detach).is_err() {
                            eof = true;
                        }
                    }

                    if eof {
                        self.eof(self.host_index - 1);
                        self.host_index -= 1;
                    }
                }

                self.host_index = (self.host_index + 1) % (self.clients.len() + 1);

                if self.host_index > 0 {
                    let mut eof = false;

                    if let Some(new_client) = self.clients.get_mut(self.host_index - 1) {
                        if write_packet(
                            &mut new_client.stream,
                            &Packet::Attach {
                                busids: busids.to_owned(),
                            },
                        )
                        .is_err()
                        {
                            eof = true;
                        }
                    }

                    if eof {
                        self.eof(self.host_index - 1);
                        self.host_index = 0;
                    }
                }
            }

            thread::sleep(time::Duration::from_millis(10));
        }
    }

    fn eof(&mut self, index: usize) {
        let address = self.clients[index].address;
        self.clients.remove(index);
        println!("stream closed: {}", address);
    }
}

impl Client {
    pub fn connect(addr: &str, port: u16, usbip_port: u16) -> Result<Client, NetError> {
        let sockaddr = SocketAddr::new(addr.parse()?, port);
        let stream = TcpStream::connect(sockaddr)?;
        stream.set_nonblocking(true)?;

        println!("connected to remote host: {}", sockaddr);

        Ok(Client {
            address: sockaddr,
            stream,
            usbip_port,
        })
    }

    pub fn attach(&self, usbip_binary: &str, busids: &[String]) -> Result<(), NetError> {
        for busid in busids {
            Command::new(usbip_binary)
                .args([
                    "--tcp-port",
                    &self.usbip_port.to_string(),
                    "attach",
                    "-r",
                    &self.address.ip().to_string(),
                    "-b",
                    &busid,
                ])
                .output()?;
        }
        Ok(())
    }

    pub fn detach(usbip_binary: &str) -> Result<(), NetError> {
        let stdout = String::from_utf8(Command::new(usbip_binary).args(["port"]).output()?.stdout)?;

        let ports: Vec<&str> = stdout
            .lines()
            .map(|line| line.trim())
            .filter(|line| line.starts_with("Port"))
            .filter_map(|line| {
                line.split_whitespace()
                    .nth(1)
                    .map(|p| p.trim_end_matches(':'))
            })
            .collect();

        for port in ports {
            Command::new(usbip_binary)
                .args(["detach", "-p", &port])
                .output()?;
        }

        Ok(())
    }

    pub fn run(&mut self, receiver: Receiver<Event>, usbip_binary: &str) -> Result<(), NetError> {
        let res = self.run_loop(receiver, usbip_binary);
        Self::detach(usbip_binary).ok();
        res
    }

    fn run_loop(&mut self, receiver: Receiver<Event>, usbip_binary: &str) -> Result<(), NetError> {
        loop {
            while let Ok(Some(packet)) = read_packet(&mut self.stream) {
                match packet {
                    Packet::Attach { busids } => self.attach(usbip_binary, &busids)?,
                    Packet::Detach => Self::detach(usbip_binary)?,
                    _ => {}
                }
            }

            while let Ok(event) = receiver.try_recv() {
                match event {
                    Event::Activated => {
                        println!("swapping hosts now...");
                        self.activate()?;
                    }
                }
            }

            thread::sleep(time::Duration::from_millis(10));
        }
    }

    fn activate(&mut self) -> Result<(), NetError> {
        let packet = Packet::Activated;
        write_packet(&mut self.stream, &packet)
    }
}
