//! Generated gRPC/protobuf types from `proto/patok/v1/engine.proto`, plus the gRPC
//! client's Unix-socket connect helper.

/// Generated code: lint noise by design, suppressed only here.
pub mod generated {
    #![allow(clippy::all, clippy::pedantic)]

    tonic::include_proto!("patok.v1");
}

pub use generated::*;

pub mod client;
