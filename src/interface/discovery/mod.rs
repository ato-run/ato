//! Service Discovery
//!
//! mDNS announcer for .local domain advertisement (Dev/Desktop integration)

pub mod mdns;

pub use mdns::MdnsAnnouncer;
