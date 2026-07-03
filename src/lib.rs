#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

mod date;
mod enc;
mod generated;
mod request;
mod response;
mod server;
mod sse;
mod threadpool;
mod types;

pub use crate::{
    request::Request,
    response::Response,
    server::{AnyResponse, Config, serve, serve_graceful},
    sse::{SseResponse, SseWriter},
    types::*,
};
