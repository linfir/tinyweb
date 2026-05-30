#![forbid(unsafe_code)]
#![doc = include_str!("../README.md")]

mod enc;
mod generated;
mod request;
mod response;
mod server;
mod sse;
mod types;

pub use crate::{
    request::Request,
    response::Response,
    server::{AnyResponse, Config, serve},
    sse::{SseResponse, SseWriter},
    types::*,
};
