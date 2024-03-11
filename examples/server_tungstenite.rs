use std::net::TcpListener;
use std::thread::spawn;
use tungstenite::protocol::frame::coding::{Control, Data, OpCode};
use tungstenite::protocol::frame::{Frame, FrameHeader};
use tungstenite::{accept, Message};

/// A WebSocket echo server
fn main() {
    let server = TcpListener::bind("192.168.1.196:4443").unwrap();
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
