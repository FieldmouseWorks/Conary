//! Bounded HTTP transport. Ingress never runs a flow pass on its request thread.
use conary_github::events::{MAX_BODY, Queue, Receiver};
use std::io;
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};

use crate::http;

pub fn serve(listener: TcpListener, receiver: Receiver, queue: Arc<Queue>) -> io::Result<()> {
    let receiver = Mutex::new(receiver);
    http::serve(listener, 64, move |mut stream| {
        let status = request(&mut stream, &receiver, &queue).unwrap_or(400);
        let _ = http::respond(&mut stream, status, None);
    })
}

fn request(stream: &mut TcpStream, receiver: &Mutex<Receiver>, queue: &Queue) -> io::Result<u16> {
    // Unsigned deliveries are refused before their body is read.
    let admit = |headers: &http::Headers| match headers.contains_key("x-hub-signature-256") {
        true => Ok(()),
        false => Err(401),
    };
    let request = match http::read(stream, MAX_BODY, admit)? {
        Ok(request) => request,
        Err(status) => return Ok(status),
    };
    let header = |name: &str| request.headers.get(name).map(String::as_str);
    Ok(receiver.lock().unwrap().receive(
        header("x-hub-signature-256"),
        header("x-github-delivery").unwrap_or(""),
        header("x-github-event").unwrap_or(""),
        &request.body,
        conary_github::flow::jobs::now(),
        queue,
    ))
}
