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
    Utf8Error(#[from] string::FromUtf8Error),
}

pub enum Event {
    Activated,
    Eof(SocketAddr),
}

#[derive(Debug, SchemaWrite, SchemaRead)]
pub enum Packet {
    Activated,
    Attach { busids: Vec<String> },
    Detach,
}

pub struct Server {
    addresses: Vec<SocketAddr>,
    receiver: mpsc::Receiver<Event>,
    listener: TcpListener,
    clients: Vec<Client>,
    host_index: usize,
}

pub struct Client {
    pub address: SocketAddr,
    pub stream: TcpStream,
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
                    println!("read packet -> {:?}: {:?}", stream.peer_addr(), packet);
                    return Ok(Some(packet));
                }
                Ok(_) => {}
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(NetError::Io(e)),
            }
        }
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

    println!("wrote packet -> {:?}: {:?}", stream.peer_addr(), packet);

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

        Ok((
            Server {
                addresses,
                receiver: channel.1,
                listener,
                clients: Vec::new(),
                host_index: 0,
            },
            channel.0,
        ))
    }

    pub fn addresses(&self) -> &[SocketAddr] {
        &self.addresses
    }

    pub fn run(&mut self, usbipd_binary: &str, busids: &[String]) -> Result<(), NetError> {
        #[cfg(target_os = "windows")]
        for busid in busids {
            Command::new(usbipd_binary).args(["bind", busid]).output()?;
        }

        loop {
            let mut cycle_host: bool = false;

            match self.listener.accept() {
                Ok((s, addr)) => {
                    s.set_nonblocking(true)?;
                    self.clients.push(Client {
                        address: addr,
                        stream: s,
                    });
                    println!("stream opened: {}", addr);
                }
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => {}
                Err(e) => return Err(NetError::Io(e)),
            }

            if self.host_index > 0 {
                let client = &mut self.clients[self.host_index - 1];

                while let Ok(Some(packet)) = read_packet(&mut client.stream) {
                    match packet {
                        Packet::Activated => cycle_host = true,
                        _ => {}
                    }
                }
            }

            while let Ok(event) = self.receiver.try_recv() {
                match event {
                    Event::Activated => {
                        if self.host_index == 0 {
                            cycle_host = true;
                        }
                    }
                    Event::Eof(addr) => {
                        if let Some(index) = self.clients.iter().position(|c| c.address == addr) {
                            self.clients.remove(index);
                            if self.host_index >= index + 1 {
                                cycle_host = true;
                            }
                            println!("stream closed: {}", addr);
                        }
                    }
                }
            }

            if cycle_host {
                println!("swapping hosts now...");

                let new_index = (self.host_index + 1) % (self.clients.len() + 1);
                let attach_packet = Packet::Attach {
                    busids: busids.into(),
                };
                let detach_packet = Packet::Detach;

                if self.host_index > 0 {
                    let client_a = &mut self.clients[self.host_index - 1];
                    write_packet(&mut client_a.stream, &detach_packet)?;
                }

                if new_index > 0 {
                    let client = &mut self.clients[new_index - 1];
                    write_packet(&mut client.stream, &attach_packet)?;
                }

                self.host_index = new_index;
            }

            thread::sleep(time::Duration::from_millis(10));
        }
    }
}

impl Client {
    pub fn connect(addr: &str, port: u16) -> Result<Client, NetError> {
        let sockaddr = SocketAddr::new(addr.parse()?, port);
        let stream = TcpStream::connect(sockaddr)?;
        stream.set_nonblocking(true)?;

        println!("connected to remote host: {}", sockaddr);

        Ok(Client {
            address: sockaddr,
            stream,
        })
    }

    pub fn attach(&self, usbip_binary: &str, busids: &[String]) -> Result<(), NetError> {
        for busid in busids {
            Command::new(usbip_binary)
                .args(["attach", "-r", &self.address.ip().to_string(), "-b", &busid])
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
                    Event::Eof(_) => {
                        Self::detach(usbip_binary)?;
                        break;
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
