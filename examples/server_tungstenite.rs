use std::{net::TcpListener, thread::spawn};
use tungstenite::{
    accept,
    protocol::frame::{
        coding::{Control, Data, OpCode},
        Frame, FrameHeader,
    },
    Message,
};

#[derive(argh::FromArgs)]
#[argh(description = "server example")]
struct Args {
    #[argh(option, description = "address to bind to")]
    bind: String,
}

/// A WebSocket echo server
fn main() {
    let args: Args = argh::from_env();

    let server = TcpListener::bind(&args.bind).unwrap();
    for stream in server.incoming() {
        spawn(move || {
            println!("got client");
            let mut websocket = accept(stream.unwrap()).unwrap();
            println!("accepted handshake");
            websocket
                .write(Message::Frame(Frame::from_payload(
                    FrameHeader {
                        is_final: false,
                        opcode: OpCode::Data(Data::Text),
                        ..Default::default()
                    },
                    String::from("dumpty").into_bytes(),
                )))
                .unwrap();

            websocket
                .write(Message::Frame(Frame::from_payload(
                    FrameHeader {
                        is_final: true,
                        opcode: OpCode::Control(Control::Ping),
                        ..Default::default()
                    },
                    Vec::with_capacity(0),
                )))
                .unwrap();
            websocket
                .write(Message::Frame(Frame::from_payload(
                    FrameHeader {
                        is_final: true,
                        opcode: OpCode::Control(Control::Ping),
                        ..Default::default()
                    },
                    String::from("Hello").into_bytes(),
                )))
                .unwrap();

            websocket
                .write(Message::Frame(Frame::from_payload(
                    FrameHeader {
                        is_final: true,
                        opcode: OpCode::Control(Control::Ping),
                        ..Default::default()
                    },
                    Vec::with_capacity(0),
                )))
                .unwrap();

            websocket
                .write(Message::Frame(Frame::from_payload(
                    FrameHeader {
                        is_final: true,
                        opcode: OpCode::Control(Control::Ping),
                        ..Default::default()
                    },
                    Vec::with_capacity(0),
                )))
                .unwrap();

            websocket
                .write(Message::Frame(Frame::from_payload(
                    FrameHeader {
                        is_final: true,
                        opcode: OpCode::Control(Control::Ping),
                        ..Default::default()
                    },
                    Vec::with_capacity(0),
                )))
                .unwrap();

            websocket
                .write(Message::Frame(Frame::from_payload(
                    FrameHeader {
                        is_final: false,
                        opcode: OpCode::Data(Data::Continue),
                        ..Default::default()
                    },
                    String::from("donkey").into_bytes(),
                )))
                .unwrap();
            websocket
                .write(Message::Frame(Frame::from_payload(
                    FrameHeader {
                        is_final: true,
                        opcode: OpCode::Control(Control::Ping),
                        ..Default::default()
                    },
                    Vec::with_capacity(0),
                )))
                .unwrap();

            websocket
                .write(Message::Frame(Frame::from_payload(
                    FrameHeader {
                        is_final: true,
                        opcode: OpCode::Control(Control::Ping),
                        ..Default::default()
                    },
                    Vec::with_capacity(0),
                )))
                .unwrap();

            websocket
                .write(Message::Frame(Frame::from_payload(
                    FrameHeader {
                        is_final: true,
                        opcode: OpCode::Data(Data::Continue),
                        ..Default::default()
                    },
                    String::from("dooby").into_bytes(),
                )))
                .unwrap();
            websocket
                .write(Message::Frame(Frame::from_payload(
                    FrameHeader {
                        is_final: true,
                        opcode: OpCode::Control(Control::Ping),
                        ..Default::default()
                    },
                    Vec::with_capacity(0),
                )))
                .unwrap();

            websocket.flush().unwrap();

            println!("sent hello");

            //let mesg = websocket.read().unwrap();
            //println!("client sent {:?}", mesg);

            websocket.close(None).unwrap();
            let mut msg = websocket.read().unwrap();
            while !msg.is_close() {
                println!("closing client sent {:?}", msg);
                msg = websocket.read().unwrap();
            }

            println!("done");
        });
    }
}
