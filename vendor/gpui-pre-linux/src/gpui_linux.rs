#![cfg(any(target_os = "linux", target_os = "freebsd"))]
mod linux;

pub use linux::current_platform;
// murmur patch
pub use linux::set_next_activation_token;
