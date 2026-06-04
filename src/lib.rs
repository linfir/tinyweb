#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

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
    server::{AnyResponse, Config, ServeError, serve},
    sse::{SseResponse, SseWriter},
    types::*,
};
