pub mod admin {
    pub mod backup;
    pub mod config;
    pub mod db;
    pub mod init;
    pub mod plugin;
    pub mod tools;
}

pub mod observability {
    pub mod events;
    pub mod files;
    pub mod health;
    pub mod history;
    pub mod jobs;
    pub mod plans;
    pub mod report;
    pub mod serve;
}

pub mod shell {
    pub mod completions;
    pub mod since;
}

pub mod workflow {
    pub mod inspect;
    pub mod paths;
    pub mod policy;
    pub mod process;
    pub mod progress;
    pub mod scan;
    pub mod verify;
}

pub use admin::{backup, config, db, init, plugin, tools};
pub use observability::{events, files, health, history, jobs, plans, report, serve};
pub use shell::{completions, since};
pub use workflow::{inspect, policy, process, scan, verify};
